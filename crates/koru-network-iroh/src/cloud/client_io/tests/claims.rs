// SPDX-License-Identifier: MPL-2.0

//! The claim reference through the client carrier (TECHNICAL §5.4 items 4
//! and 6): a connection's upload writes it into its start and the worker
//! hands it on; a device's client cannot send one; no client accepts one on
//! a start it receives.

use super::*;
use koru_replication::cloud::transfer::ClaimReferences;

const CLAIM: [u8; 32] = [0x5a; 32];

fn claimed() -> UploadContext {
    UploadContext { claim: Some(CLAIM) }
}

fn identity(scope: Scope) -> Unit {
    Unit::Identity {
        profile_id: scope.profile,
    }
}

/// Read records at the worker end until the upload is complete, and hand it
/// off. Answers the start the client wrote with the handoff.
async fn receive_upload(
    reader: &mut ReadHalf<DuplexStream>,
    stream: &mut Stream,
    budget: &MemoryBudget,
) -> (Start, Completed) {
    let mut start = None;
    loop {
        let received = records::read(reader, Instant::now() + Duration::from_secs(10), budget)
            .await
            .unwrap();
        match received.into_parts().1 {
            Record::Control(control) => {
                if let Control::TransferStart(value) = &control {
                    start = Some(value.clone());
                }
                stream.receive_control(control, 0, 0, |_| Ok(())).unwrap();
            }
            Record::Peer(frame) => {
                stream.receive_peer(frame, 0, |_| Ok(())).unwrap();
            }
        }
        if stream.status().incoming_complete {
            let complete = stream.handoff(0, |_| Ok(()), |v| v).unwrap();
            return (start.expect("a start precedes the payload"), complete);
        }
    }
}

#[tokio::test]
async fn a_connections_upload_carries_its_reference_to_the_workers_handoff() {
    let (mut client, mut reader, _writer, mut stream, scope, budget) =
        pair_with_claims(false, ClaimReferences::Sent, ClaimReferences::Received).await;
    let bytes = vec![3; 512];
    client
        .upload_with(identity(scope), bytes.clone(), (), claimed())
        .unwrap();
    let (start, done) = drive(
        &mut client,
        receive_upload(&mut reader, &mut stream, &budget),
    )
    .await;
    assert!(start.claim == Some(Bytes(CLAIM)));
    assert_eq!(done.claim, Some(CLAIM));
    assert_eq!(done.bytes, bytes);
}

#[tokio::test]
async fn an_upload_without_a_reference_writes_none() {
    // `upload` is `upload_with` and an empty context: what a device's Store,
    // and a connection's unclaimed document, always send.
    let (mut client, mut reader, _writer, mut stream, scope, budget) =
        pair_with_claims(false, ClaimReferences::Sent, ClaimReferences::Received).await;
    client.upload(identity(scope), vec![3; 512], ()).unwrap();
    let (start, done) = drive(
        &mut client,
        receive_upload(&mut reader, &mut stream, &budget),
    )
    .await;
    assert!(start.claim.is_none());
    assert_eq!(done.claim, None);
}

#[tokio::test]
async fn a_devices_client_refuses_a_reference_before_anything_is_written() {
    let (mut client, _reader, _writer, _stream, scope, _budget) =
        pair_with_claims(false, ClaimReferences::None, ClaimReferences::None).await;
    assert_eq!(
        client
            .upload_with(identity(scope), vec![3; 512], (), claimed())
            .err(),
        Some(Error::Invalid)
    );
    let status = client.status();
    assert_eq!(status.transfer.closed, Some(Error::Invalid));
    assert_eq!(status.transfer.outgoing_bytes, 0);
    assert!(!status.writing);
}

#[tokio::test]
async fn a_client_refuses_a_start_that_carries_a_reference() {
    // A worker never sends one (C05). Even a connection's client, which
    // sends references, refuses one on a download start, and the session
    // ends before any payload is admitted.
    for end in [ClaimReferences::Sent, ClaimReferences::None] {
        let (mut client, _reader, mut writer, _stream, scope, budget) =
            pair_with_claims(false, end, ClaimReferences::None).await;
        // A misbehaving peer: a stream that may send references.
        let accepted = PreparedSession {
            accepted: HandshakeV1::SessionAccepted {
                version: Version1,
                session_id: Bytes([1; 32]),
                generation: 1,
                expires_at_seconds: 600,
            },
            principal: scope.principal,
            expires_at_seconds: 600,
            generation: 1,
            claim_references: ClaimReferences::Sent,
        };
        let mut peer = Stream::new(
            &accepted,
            scope,
            0,
            0,
            transfer::MAX_PAYLOAD,
            budget.clone(),
        )
        .unwrap();
        let own = peer.initial_policy(0).unwrap();
        peer.receive_control(own, 0, 0, |_| Ok(())).unwrap();
        let (start, _) = peer
            .begin_upload(
                identity(scope),
                [12; 8],
                vec![1; 100],
                claimed(),
                0,
                0,
                |_| Ok(()),
            )
            .unwrap();
        records::write(
            &mut writer,
            &Record::Control(start),
            Instant::now() + Duration::from_secs(10),
            &budget,
        )
        .await
        .unwrap();
        let refused = loop {
            match client.next().await {
                Ok(_) => continue,
                Err(error) => break error,
            }
        };
        assert_eq!(refused, Error::Invalid, "{end:?}");
        assert!(client.stream.incoming_deadline().is_none());
        assert!(!client.status().committing);
    }
}
