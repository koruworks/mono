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

#[tokio::test]
async fn missing_local_source_downloads_to_detached_memory_without_ack_or_store() {
    let (mut client, mut reader, mut writer, mut stream, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    client.receive = Box::new(|_| panic!("detached read cannot import into Store"));
    let unit = unit(scope);
    let doc = loro::LoroDoc::new();
    doc.get_text("body")
        .insert(0, "Cold unfiled Cloud source")
        .unwrap();
    let version = doc.oplog_vv().encode();
    let bytes = doc.export(loro::ExportMode::Snapshot).unwrap();
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
        assert!(request.known_version.is_none());
        records::write(
            &mut writer,
            &request.response(scope, version.clone()).unwrap().unwrap(),
            deadline,
            &budget,
        )
        .await
        .unwrap();
        let (start, admission) = stream
            .begin_outgoing(unit, [7; 8], bytes, 0, 0, |_| Ok(()))
            .unwrap();
        assert_eq!(admission, Admission::Automatic);
        records::write(&mut writer, &Record::Control(start), deadline, &budget)
            .await
            .unwrap();
        while let Some(record) = stream.next_outgoing(0, |_| Ok(())).unwrap() {
            records::write(&mut writer, record.record(), deadline, &budget)
                .await
                .unwrap();
        }
        stream.outbound_written(0).unwrap();
        // The peer sees close, never a success Ack for ephemeral inspection.
        assert!(records::read(&mut reader, deadline, &budget).await.is_err());
    };
    let device = async move {
        let result = download(&mut client, unit).await;
        drop(client);
        result
    };
    let (read, ()) = tokio::join!(device, peer);
    let read = read.unwrap();
    let detached = read.replay().unwrap();
    assert_eq!(
        detached.get_text("body").to_string(),
        "Cold unfiled Cloud source"
    );
    assert!(read.observed().matches(
        scope,
        unit,
        koru_store_core::sync_digest::revision(&version),
        &detached.oplog_vv().encode()
    ));
    assert_eq!(format!("{read:?}"), "ObservedDownload(<redacted>)");
}

#[tokio::test]
async fn detached_replay_refuses_corrupt_pending_and_wrong_version_payloads() {
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, "ancestor").unwrap();
    let prior = doc.oplog_vv();
    let first = doc.export(loro::ExportMode::Snapshot).unwrap();
    doc.get_text("body").insert(8, " child").unwrap();
    let version = doc.oplog_vv().encode();
    for bytes in [
        vec![255],
        doc.export(loro::ExportMode::updates(&prior)).unwrap(),
        first,
    ] {
        let (mut client, mut reader, mut writer, mut stream, scope, budget) =
            super::super::tests::pair_with_workspace(true).await;
        client.receive = Box::new(|_| panic!("invalid detached data must never enter Store"));
        let unit = unit(scope);
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
            records::write(
                &mut writer,
                &request.response(scope, version.clone()).unwrap().unwrap(),
                deadline,
                &budget,
            )
            .await
            .unwrap();
            let (start, _) = stream
                .begin_outgoing(unit, [7; 8], bytes, 0, 0, |_| Ok(()))
                .unwrap();
            records::write(&mut writer, &Record::Control(start), deadline, &budget)
                .await
                .unwrap();
            while let Some(record) = stream.next_outgoing(0, |_| Ok(())).unwrap() {
                records::write(&mut writer, record.record(), deadline, &budget)
                    .await
                    .unwrap();
            }
        };
        let (read, ()) = tokio::join!(download(&mut client, unit), peer);
        assert!(matches!(read.unwrap().replay(), Err(Error::Invalid)));
    }
}

#[tokio::test]
async fn detached_read_never_takes_over_an_existing_partial_transfer() {
    let (mut client, _reader, mut writer, mut stream, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let unit = unit(scope);
    let (start, _) = stream
        .begin_outgoing(unit, [7; 8], vec![1], 0, 0, |_| Ok(()))
        .unwrap();
    records::write(
        &mut writer,
        &Record::Control(start),
        Instant::now() + Duration::from_secs(2),
        &budget,
    )
    .await
    .unwrap();
    assert!(matches!(client.next().await.unwrap(), Event::Progress));
    assert!(client.stream.incoming_deadline().is_some());
    assert!(matches!(
        download(&mut client, unit).await,
        Err(Error::Unavailable)
    ));
    assert!(!client.inspection_only);
}

#[tokio::test]
async fn detached_read_does_not_take_over_a_receipt_already_moved_into_io() {
    let (mut client, _reader, _writer, _stream, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let unit = unit(scope);
    let (a, mut blocked_peer) = tokio::io::duplex(1);
    let (reader, writer) = tokio::io::split(a);
    let deadline = client.instant(client.stream.expires_at());
    client.io = Duplex::new(reader, writer, deadline, budget.clone());
    // This is the state after advance() moved the validated receipt into IO:
    // neither Option remains, but the Ack has not completed its framed write.
    client
        .io
        .send(scope.committed_ack(unit, [1; 8]).unwrap(), |v| v, deadline)
        .unwrap();
    client.writing = Some(Writing::Receipt);
    client.writing_deadline = Some(client.stream.expires_at());
    std::future::poll_fn(|cx| {
        let mut pending = Box::pin(client.next());
        assert!(pending.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(client.commit.is_none() && client.receipt.is_none());
    assert!(client.status().committing);
    assert!(matches!(
        download(&mut client, unit).await,
        Err(Error::Unavailable)
    ));
    assert!(!client.inspection_only);
    assert!(matches!(client.writing, Some(Writing::Receipt)));
    let sync = super::super::reconcile::Reconciler::new(
        client,
        |_| {
            Ok(async {
                Ok(super::super::reconcile::Answer::Ready(
                    loro::VersionVector::default(),
                ))
            })
        },
        |_| Ok(async { Ok(super::super::reconcile::Answer::Ready(None)) }),
    );
    let mut sync = match sync.try_inspection() {
        Err(sync) => sync,
        Ok(_) => panic!("busy carrier admitted"),
    };
    let peer = async {
        let ack = records::read(&mut blocked_peer, deadline, &budget)
            .await
            .unwrap();
        let (_, Record::Peer(frame), _) = ack.into_parts() else {
            panic!()
        };
        assert!(matches!(
            metadata::decode(scope, frame, 1).unwrap().as_slice(),
            [Metadata::Ack { ok: true, .. }]
        ));
    };
    let normal = async {
        while sync.status().transport.committing {
            sync.next().await.unwrap();
        }
    };
    tokio::join!(peer, normal);
    assert!(!sync.status().transport.committing);
}

#[tokio::test]
async fn explicit_edit_uses_ordinary_delta_and_requires_exact_remote_ack() {
    for mode in 0..4 {
        let (mut client, mut reader, mut writer, mut stream, scope, budget) =
            super::super::tests::pair_with_workspace(true).await;
        client.receive = Box::new(|_| panic!("online Cloud edit cannot import into Store"));
        let revoked = std::sync::Arc::new(AtomicBool::new(false));
        let authority = revoked.clone();
        client.authorize = Box::new(move |_| {
            if authority.load(Ordering::SeqCst) {
                Err(Refusal::DeviceRevoked)
            } else {
                Ok(())
            }
        });
        let unit = unit(scope);
        let remote = loro::LoroDoc::new();
        remote.get_text("body").insert(0, "Cloud").unwrap();
        let version = remote.oplog_vv().encode();
        let head = koru_store_core::sync_digest::revision(&version);
        let bytes = remote.export(loro::ExportMode::Snapshot).unwrap();
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
            records::write(
                &mut writer,
                &request.response(scope, version).unwrap().unwrap(),
                deadline,
                &budget,
            )
            .await
            .unwrap();
            let (start, _) = stream
                .begin_outgoing(unit, [7; 8], bytes, 0, 0, |_| Ok(()))
                .unwrap();
            records::write(&mut writer, &Record::Control(start), deadline, &budget)
                .await
                .unwrap();
            while let Some(record) = stream.next_outgoing(0, |_| Ok(())).unwrap() {
                records::write(&mut writer, record.record(), deadline, &budget)
                    .await
                    .unwrap();
            }
            stream.outbound_written(0).unwrap();
            // First response is the independent upload Start, never a false
            // durable Ack for the detached download.
            let upload = records::read(&mut reader, deadline, &budget).await;
            if mode >= 2 {
                assert!(
                    upload.is_err(),
                    "no upload after stale source or revocation"
                );
                return;
            }
            let upload = upload.unwrap();
            let (_, Record::Control(control @ Control::TransferStart(_)), _) = upload.into_parts()
            else {
                panic!("expected upload start, not receive Ack")
            };
            assert_eq!(
                stream.receive_control(control, 0, 0, |_| Ok(())).unwrap(),
                Admission::Automatic
            );
            while !stream.status().incoming_complete {
                let received = records::read(&mut reader, deadline, &budget).await.unwrap();
                let (_, Record::Peer(frame), _) = received.into_parts() else {
                    panic!()
                };
                stream.receive_peer_owned(frame, 0, |_| Ok(())).unwrap();
            }
            let complete = stream.handoff(0, |_| Ok(()), |value| value).unwrap();
            remote
                .get_text("other")
                .insert(0, "concurrent unrelated edit")
                .unwrap();
            remote.import(&complete.bytes).unwrap();
            let ack = scope
                .committed_ack(
                    unit,
                    if mode == 1 {
                        [99; 8]
                    } else {
                        complete.batch_id
                    },
                )
                .unwrap();
            records::write(&mut writer, &ack, deadline, &budget)
                .await
                .unwrap();
        };
        let edit = InspectionCarrier { client }.edit(
            unit,
            if mode == 2 { [0; 32] } else { head },
            move |source| {
                assert_ne!(mode, 2, "stale source cannot enter domain preparation");
                if mode == 3 {
                    revoked.store(true, Ordering::SeqCst);
                }
                let doc = source.replay()?;
                let before = doc.oplog_vv();
                doc.get_text("body")
                    .insert(5, " edited")
                    .map_err(|_| Error::Invalid)?;
                let delta = doc
                    .export(loro::ExportMode::updates(&before))
                    .map_err(|_| Error::Invalid)?;
                Ok((delta, source))
            },
        );
        let (result, ()) = tokio::join!(edit, peer);
        assert_eq!(
            result,
            match mode {
                0 => Ok(()),
                1 => Err(EditError::OutcomeUnknown),
                2 => Err(EditError::SourceChanged),
                _ => Err(EditError::Unavailable),
            }
        );
        if mode >= 2 {
            assert_eq!(remote.get_text("body").to_string(), "Cloud");
            continue;
        }
        assert_eq!(remote.get_text("body").to_string(), "Cloud edited");
        assert_eq!(
            remote.get_text("other").to_string(),
            "concurrent unrelated edit"
        );
    }
}
