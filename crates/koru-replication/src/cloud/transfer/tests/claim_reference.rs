// SPDX-License-Identifier: MPL-2.0

//! The claim reference end to end on the carrier (TECHNICAL §5.4 item 6): a
//! connection's upload carries it from its start to the worker's
//! [`Completed`], every other session refuses it, and a device's frames are
//! the bytes they were before the amendment.

use super::*;

/// What a device's 300 000-byte transfer (two fragments, batch
/// `[9, 8, 7, 6, 5, 4, 3, 2]`) wrote before the amendment, captured from the
/// encoder at `oss` c99d447: the `TransferStart` record, and the SHA-256 of
/// every record of the transfer, that start and its payload frames, in order.
const DEVICE_START_BEFORE: &str = "0000013f017b226b696e64223a227472616e736665725f7374617274222c2276657273696f6e223a312c22706f6c6963795f67656e65726174696f6e223a312c22756e6974223a7b226b696e64223a22646f63756d656e74222c22776f726b73706163655f6964223a2230313031303130312d303130312d343130312d383130312d303130313031303130313031222c22646f63756d656e745f6964223a2230323032303230322d303230322d343230322d383230322d303230323032303230323032227d2c2262617463685f6964223a5b392c382c372c362c352c342c332c325d2c22746f74616c5f6c656e677468223a3330303030302c22667261676d656e745f636f756e74223a322c22736861323536223a22504758716b304a4b6e4459763741343661656f324178364b4e595242523533575a637868454f712d657767227d";
const DEVICE_TRANSFER_BEFORE: (usize, &str) = (
    1_069_674,
    "e805b5a3fb255469bf433cc521ea46f2536786dfb9fc569a1c68e9879ed8408d",
);
const BATCH: [u8; 8] = [9, 8, 7, 6, 5, 4, 3, 2];
const CLAIM: [u8; 32] = [0x27; 32];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn payload() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

fn scope() -> Scope {
    Scope {
        principal: CloudReplicaPrincipalV1::Workspace {
            workspace_id: id(1),
            incarnation_id: id(3),
            logical_replica_id: id(4),
        },
        eligibility_workspace: ws(),
        profile: id(5),
        manifest: doc(),
    }
}

/// Two ends of one session that have exchanged policies.
fn session(client: ClaimReferences, worker: ClaimReferences) -> (Stream, Stream) {
    let (mut a, _) = setup_claims(MAX_PAYLOAD, scope(), client);
    let (mut b, _) = setup_claims(MAX_PAYLOAD, scope(), worker);
    let ap = a.initial_policy(0).unwrap();
    let bp = b.initial_policy(0).unwrap();
    a.receive_control(bp, 0, 0, |_| Ok(())).unwrap();
    b.receive_control(ap, 0, 0, |_| Ok(())).unwrap();
    (a, b)
}

/// Run one upload from `sender` to `receiver` and answer every record it
/// wrote, encoded, with the receiver's `Completed`.
fn transfer(
    sender: &mut Stream,
    receiver: &mut Stream,
    context: UploadContext,
) -> (Vec<Vec<u8>>, Completed) {
    let (start, admission) = sender
        .begin_upload(unit(), BATCH, payload(), context, 1, 1, |_| Ok(()))
        .unwrap();
    assert_eq!(admission, Admission::Automatic);
    let mut records = vec![encode_record(&Record::Control(start.clone())).unwrap()];
    receiver.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    while let Some(record) = sender.next_outgoing(2, |_| Ok(())).unwrap() {
        records.push(encode_record(record.record()).unwrap());
        let Record::Peer(frame) = record.record() else {
            panic!("a payload frame")
        };
        receiver.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap();
    }
    sender.outbound_written(3).unwrap();
    let complete = receiver
        .handoff(3, |_| Ok(()), |complete| complete)
        .unwrap();
    (records, complete)
}

#[test]
fn a_devices_start_and_frames_are_byte_identical_to_before_the_amendment() {
    let (mut device, mut worker) = session(ClaimReferences::None, ClaimReferences::None);
    let (records, complete) = transfer(&mut device, &mut worker, UploadContext::default());
    assert_eq!(hex(&records[0]), DEVICE_START_BEFORE);
    let wire = records.concat();
    assert_eq!(
        (wire.len(), hex(&Sha256::digest(&wire))),
        (
            DEVICE_TRANSFER_BEFORE.0,
            DEVICE_TRANSFER_BEFORE.1.to_owned()
        )
    );
    // The handoff a device's upload makes is the one it always made, and it
    // carries no reference.
    assert_eq!(complete.unit, unit());
    assert_eq!(complete.batch_id, BATCH);
    assert_eq!(complete.bytes, payload());
    assert_eq!(complete.claim, None);
}

#[test]
fn a_connections_unclaimed_upload_writes_a_devices_bytes() {
    // C02: an unclaimed document's upload from a connection carries none, and
    // is the same start a device writes.
    let (mut connection, mut worker) = session(ClaimReferences::Sent, ClaimReferences::Received);
    let (records, complete) = transfer(&mut connection, &mut worker, UploadContext::default());
    assert_eq!(hex(&records[0]), DEVICE_START_BEFORE);
    assert_eq!(complete.claim, None);
}

#[test]
fn a_connections_reference_rides_from_its_start_to_the_workers_handoff() {
    let (mut connection, mut worker) = session(ClaimReferences::Sent, ClaimReferences::Received);
    let (records, complete) = transfer(
        &mut connection,
        &mut worker,
        UploadContext { claim: Some(CLAIM) },
    );
    // The start is the device's with one property more, written last.
    let expected = format!(
        "{},\"claim\":\"{}\"}}",
        std::str::from_utf8(&hex_bytes(DEVICE_START_BEFORE)[5..])
            .unwrap()
            .strip_suffix('}')
            .unwrap(),
        serde_json::to_value(Bytes(CLAIM))
            .unwrap()
            .as_str()
            .unwrap()
    );
    assert_eq!(std::str::from_utf8(&records[0][5..]).unwrap(), expected);
    // The payload frames do not change.
    let (mut device, mut device_worker) = session(ClaimReferences::None, ClaimReferences::None);
    let (device_records, _) = transfer(&mut device, &mut device_worker, UploadContext::default());
    assert_eq!(records[1..], device_records[1..]);
    // The worker hands the reference on with the upload's retained work.
    assert_eq!(complete.claim, Some(CLAIM));
    assert_eq!(complete.bytes, payload());
}

fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn claimed_start() -> Control {
    let (mut connection, _) = session(ClaimReferences::Sent, ClaimReferences::Received);
    let (start, _) = connection
        .begin_upload(
            unit(),
            BATCH,
            vec![7; 64],
            UploadContext { claim: Some(CLAIM) },
            1,
            1,
            |_| Ok(()),
        )
        .unwrap();
    start
}

#[test]
fn a_reference_on_any_session_but_a_connections_upload_is_invalid() {
    // A device's session at either end (C04), and a connection's own client,
    // which never receives one (C05): each refuses the start before it can
    // bind or admit anything, and the stream ends.
    for end in [ClaimReferences::None, ClaimReferences::Sent] {
        let (_, mut receiver) = session(ClaimReferences::Sent, end);
        assert_eq!(
            receiver.receive_control(claimed_start(), 1, 1, |_| Ok(())),
            Err(Error::Invalid)
        );
        let status = receiver.status();
        assert_eq!(status.closed, Some(Error::Invalid));
        assert_eq!(status.incoming_bytes, 0);
    }
    // The worker serving the connection admits it.
    let (_, mut worker) = session(ClaimReferences::Sent, ClaimReferences::Received);
    assert_eq!(
        worker.receive_control(claimed_start(), 1, 1, |_| Ok(())),
        Ok(Admission::Automatic)
    );
}

#[test]
fn only_a_connections_client_may_send_a_reference() {
    // A device's Store never sets one; if anything did, the device's stream
    // refuses before a start exists. A worker's own starts never carry one.
    for end in [ClaimReferences::None, ClaimReferences::Received] {
        let (mut sender, _) = session(end, ClaimReferences::Received);
        assert_eq!(
            sender
                .begin_upload(
                    unit(),
                    BATCH,
                    vec![7; 64],
                    UploadContext { claim: Some(CLAIM) },
                    1,
                    1,
                    |_| Ok(()),
                )
                .err(),
            Some(Error::Invalid)
        );
        assert_eq!(sender.status().outgoing_bytes, 0);
    }
}

#[test]
fn the_reference_is_never_printed() {
    let Control::TransferStart(start) = claimed_start() else {
        panic!("a start")
    };
    let printed = format!("{start:?} {:?}", UploadContext { claim: Some(CLAIM) });
    let encoded = serde_json::to_value(Bytes(CLAIM)).unwrap();
    assert!(!printed.contains(encoded.as_str().unwrap()), "{printed}");
    assert!(!printed.contains("39, 39"), "{printed}");
    assert!(!printed.contains("2727"), "{printed}");
}

#[test]
fn the_session_principal_decides_each_end() {
    use crate::cloud::SessionPrincipal;
    let device = SessionPrincipal::Device { device_id: id(9) };
    let connection = SessionPrincipal::AppConnection {
        connection_id: serde_json::from_value(serde_json::to_value(id(9)).unwrap()).unwrap(),
        access: koru_account_core::workspace_authorization::AppConnectionAccess::ReadWrite,
        stage: koru_account_session::cloud::app_connection::AppConnectionStage::Full,
    };
    assert_eq!(ClaimReferences::worker(&device), ClaimReferences::None);
    assert_eq!(ClaimReferences::client(&device), ClaimReferences::None);
    assert_eq!(
        ClaimReferences::worker(&connection),
        ClaimReferences::Received
    );
    assert_eq!(ClaimReferences::client(&connection), ClaimReferences::Sent);
}
