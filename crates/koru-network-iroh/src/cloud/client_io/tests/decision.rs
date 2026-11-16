// SPDX-License-Identifier: MPL-2.0

//! The client carrier against a worker end whose policy requires its
//! decision for every start: what actually crosses the wire, and when.

use super::*;
use koru_replication::cloud::transfer::ClaimReferences;

const WAIT: Duration = Duration::from_secs(10);

async fn deciding() -> (
    TestClient,
    ReadHalf<DuplexStream>,
    WriteHalf<DuplexStream>,
    Stream,
    Scope,
    MemoryBudget,
) {
    pair_with_mode(false, ClaimReferences::None, ClaimReferences::None, true).await
}

/// The next record the client writes, while the client keeps running.
async fn read_next(
    client: &mut TestClient,
    reader: &mut ReadHalf<DuplexStream>,
    budget: &MemoryBudget,
) -> Record {
    let received = drive(client, records::read(reader, Instant::now() + WAIT, budget))
        .await
        .unwrap();
    received.into_parts().1
}

/// Run the client for a while and prove it writes nothing at all.
async fn writes_nothing(
    client: &mut TestClient,
    reader: &mut ReadHalf<DuplexStream>,
    budget: &MemoryBudget,
) {
    let quiet = drive(client, async {
        tokio::time::timeout(
            Duration::from_millis(250),
            records::read(reader, Instant::now() + WAIT, budget),
        )
        .await
        .is_err()
    })
    .await;
    assert!(quiet, "the client wrote a record before its Accept");
}

fn tiny_loro() -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.get_text("fixture").insert(0, "a").unwrap();
    doc.export(loro::ExportMode::Snapshot).unwrap()
}

#[tokio::test]
async fn the_client_advertises_its_own_policy_with_the_field_omitted() {
    let budget = MemoryBudget::new(128 * 1024 * 1024);
    let (a, b) = tokio::io::duplex(4096);
    let (ar, aw) = tokio::io::split(a);
    let (mut br, _bw) = tokio::io::split(b);
    let mut client: TestClient = Client::new(
        ar,
        aw,
        config(transfer::RESTRICTED_LIMIT, budget.clone()),
        |_| Ok(async { Err(Error::Unavailable) }),
        |_| Ok(()),
        |_| Decision::Defer,
    )
    .unwrap();
    // Nothing learned yet is not a mode.
    assert!(!client.upload_decision_required());
    let expected = transfer::encode_record(&Record::Control(Control::policy(
        1,
        transfer::RESTRICTED_LIMIT,
    )))
    .unwrap();
    let mut wire = vec![0; expected.len()];
    drive(&mut client, async {
        tokio::io::AsyncReadExt::read_exact(&mut br, &mut wire)
            .await
            .unwrap();
    })
    .await;
    assert_eq!(wire, expected);
    assert_eq!(
        &wire[5..],
        br#"{"kind":"transfer_policy","version":1,"policy_generation":1,"automatic_limit_bytes":200000}"#
    );
}

#[tokio::test]
async fn a_tiny_loro_upload_sends_only_its_start_until_the_exact_accept() {
    let (mut client, mut reader, mut writer, mut stream, scope, budget) = deciding().await;
    assert!(client.upload_decision_required());
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    let bytes = tiny_loro();
    client.upload(unit, bytes.clone(), ()).unwrap();
    let Record::Control(Control::TransferStart(start)) =
        read_next(&mut client, &mut reader, &budget).await
    else {
        panic!("the first record is the start");
    };
    assert_eq!(start.total_length, bytes.len());
    // The whole body fits one frame, and none of it is written.
    writes_nothing(&mut client, &mut reader, &budget).await;
    assert!(!client.status().awaiting_ack);
    assert_eq!(
        stream
            .receive_control(Control::TransferStart(start.clone()), 0, 0, |_| Ok(()))
            .unwrap(),
        Admission::AwaitDecision
    );
    assert_eq!(stream.status().incoming_bytes, 0);
    let accept = stream
        .decide(&start, Decision::Accept, 0, |_| Ok(()))
        .unwrap();
    records::write(
        &mut writer,
        &Record::Control(accept),
        Instant::now() + WAIT,
        &budget,
    )
    .await
    .unwrap();
    let done = loop {
        let Record::Peer(frame) = read_next(&mut client, &mut reader, &budget).await else {
            panic!("only the accepted body follows");
        };
        stream.receive_peer(frame, 0, |_| Ok(())).unwrap();
        if stream.status().incoming_complete {
            break stream.handoff(0, |_| Ok(()), |v| v).unwrap();
        }
    };
    assert_eq!(done.bytes, bytes);
    // A valid Loro body, not a framing fixture.
    let imported = loro::LoroDoc::new();
    imported.import(&done.bytes).unwrap();
    assert_eq!(imported.get_text("fixture").to_string(), "a");
    // Accept was not the receipt: the upload completes only on the Ack.
    assert!(client.upload.is_some());
    let ack = scope.committed_ack(done.unit, done.batch_id).unwrap();
    records::write(&mut writer, &ack, Instant::now() + WAIT, &budget)
        .await
        .unwrap();
    loop {
        if let Event::Uploaded { unit: uploaded, .. } = client.next().await.unwrap() {
            assert_eq!(uploaded, unit);
            break;
        }
    }
}

#[tokio::test]
async fn a_metadata_refusal_sees_no_body_frame_and_no_ack_at_either_size_bound() {
    for length in [1, transfer::MAX_PAYLOAD] {
        let (mut client, mut reader, mut writer, mut stream, scope, budget) = deciding().await;
        let unit = Unit::Identity {
            profile_id: scope.profile,
        };
        client.upload(unit, vec![7; length], ()).unwrap();
        let Record::Control(Control::TransferStart(start)) =
            read_next(&mut client, &mut reader, &budget).await
        else {
            panic!("the first record is the start");
        };
        assert_eq!(start.total_length, length);
        writes_nothing(&mut client, &mut reader, &budget).await;
        // The worker refuses on the metadata alone and reserves no body.
        let before = budget.used();
        stream
            .reject_start_admission(start, 0, 0, |_| Ok(()))
            .unwrap();
        assert_eq!(budget.used(), before);
        assert_eq!(stream.status().incoming_bytes, 0);
        records::write(
            &mut writer,
            &Record::Peer(Error::CloudStoredDocumentBytesLimit.frame()),
            Instant::now() + WAIT,
            &budget,
        )
        .await
        .unwrap();
        // The refusal is never an upload completion.
        let ended = loop {
            match client.next().await {
                Ok(Event::Uploaded { .. }) => panic!("a refusal acknowledged an upload"),
                Ok(_) => {}
                Err(error) => break error,
            }
        };
        assert_eq!(ended.code(), "cloud_stored_document_bytes_limit");
        // Whatever else the client wrote before it ended, none of it is body.
        drop(client);
        while let Ok(received) = records::read(&mut reader, Instant::now() + WAIT, &budget).await {
            assert!(
                matches!(received.record(), Record::Peer(PeerFrame::Error { .. })),
                "length {length}: a record other than the closing error followed the start"
            );
        }
    }
}

#[tokio::test]
async fn the_retry_gate_follows_only_the_current_policy_of_this_stream() {
    // An older worker: the field is absent, so the gate is closed while
    // ordinary uploads keep their automatic behavior.
    let (mut client, mut reader, _writer, _stream, scope, budget) = pair().await;
    assert!(!client.upload_decision_required());
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    client.upload(unit, tiny_loro(), ()).unwrap();
    assert!(matches!(
        read_next(&mut client, &mut reader, &budget).await,
        Record::Control(Control::TransferStart(_))
    ));
    assert!(matches!(
        read_next(&mut client, &mut reader, &budget).await,
        Record::Peer(PeerFrame::LoroBatch { .. })
    ));
    drop(client);

    // A worker that turns the mode off again: the gate closes with the new
    // generation, not with a remembered true.
    let (mut client, _reader, mut writer, mut stream, _, budget) = deciding().await;
    assert!(client.upload_decision_required());
    let off = stream
        .change_policy_deciding(transfer::MAX_PAYLOAD, false, 0)
        .unwrap();
    records::write(
        &mut writer,
        &Record::Control(off),
        Instant::now() + WAIT,
        &budget,
    )
    .await
    .unwrap();
    while client.status().transfer.peer_generation != Some(2) {
        client.next().await.unwrap();
    }
    assert!(!client.upload_decision_required());
    // The reconciler reports the same current value to its scheduler.
    let sync = reconcile::Reconciler::new(
        client,
        |_| Ok(async { Err::<reconcile::Answer<loro::VersionVector>, _>(Error::Unavailable) }),
        |_| {
            Ok(async {
                Err::<reconcile::Answer<Option<koru_replication::cloud::client_store::Export>>, _>(
                    Error::Unavailable,
                )
            })
        },
    );
    assert!(!sync.upload_decision_required());
}
