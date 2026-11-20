// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn fully_in_flight_rejected_body_never_becomes_a_handoff() {
    let (mut sender, mut receiver) = pair(MAX_PAYLOAD);
    let (Control::TransferStart(start), _) = sender
        .begin_outgoing(unit(), [6; 8], vec![1; 64], 1, 1, |_| Ok(()))
        .unwrap()
    else {
        panic!()
    };
    receiver
        .reject_start_admission(start, 1, 1, |_| Ok(()))
        .unwrap();
    while let Some(record) = sender.next_outgoing(2, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        assert_eq!(
            receiver.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap(),
            Admission::Deferred
        );
    }
    assert!(!receiver.status().incoming_complete);
    assert_eq!(receiver.status().incoming_bytes, 0);
    assert_eq!(receiver.status().closed, None);
    sender.reject_outgoing_admission(3).unwrap();
    let (start, _) = sender
        .begin_outgoing(unit(), [7; 8], vec![2; 64], 3, 3, |_| Ok(()))
        .unwrap();
    receiver.receive_control(start, 3, 3, |_| Ok(())).unwrap();
    while let Some(record) = sender.next_outgoing(4, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        receiver.receive_peer(frame.clone(), 4, |_| Ok(())).unwrap();
    }
    assert_eq!(
        receiver.handoff(5, |_| Ok(()), |done| done.bytes).unwrap(),
        vec![2; 64]
    );
}

#[test]
fn refused_automatic_body_is_bounded_and_opposite_direction_survives() {
    let (mut client, _) = setup(MAX_PAYLOAD);
    let (mut worker, budget) = setup(MAX_PAYLOAD);
    let client_policy = client.initial_policy(0).unwrap();
    let worker_policy = worker.initial_policy(0).unwrap();
    client
        .receive_control(worker_policy, 0, 0, |_| Ok(()))
        .unwrap();
    worker
        .receive_control(client_policy, 0, 0, |_| Ok(()))
        .unwrap();
    let (download, _) = worker
        .begin_outgoing(unit(), [2; 8], vec![9; 64], 1, 1, |_| Ok(()))
        .unwrap();
    client.receive_control(download, 1, 1, |_| Ok(())).unwrap();
    let before = budget.used();
    let (Control::TransferStart(start), _) = client
        .begin_outgoing(
            unit(),
            [3; 8],
            vec![7; 4 * FRAGMENT_BYTES],
            1,
            1,
            |_| Ok(()),
        )
        .unwrap()
    else {
        panic!()
    };
    worker
        .reject_start_admission(start, 1, 1, |_| Ok(()))
        .unwrap();
    assert!(
        budget.used() < before + 1024,
        "rejected payload must not be reserved"
    );
    let first = client.next_outgoing(2, |_| Ok(())).unwrap().unwrap();
    let Record::Peer(frame) = first.record() else {
        panic!()
    };
    assert_eq!(
        worker.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap(),
        Admission::Deferred
    );
    client.reject_outgoing_admission(3).unwrap();
    assert_eq!(client.status().incoming_complete, false);
    assert_eq!(worker.status().outgoing_bytes, 64);
    // The client stopped after the refusal. Ordered metadata ends the discard
    // window even when the rejected automatic body was only partly sent.
    let Record::Peer(join) =
        super::super::metadata::join_request(worker.scope(), unit(), Vec::new()).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        worker.receive_peer(join, 3, |_| Ok(())).unwrap(),
        Admission::Metadata
    );
    assert_eq!(budget.used(), before);
    while let Some(record) = worker.next_outgoing(4, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        client.receive_peer(frame.clone(), 4, |_| Ok(())).unwrap();
    }
    assert_eq!(
        client.handoff(5, |_| Ok(()), |done| done.bytes).unwrap(),
        vec![9; 64]
    );
    assert_eq!(worker.status().closed, None);
}

#[test]
fn rejected_body_still_requires_exact_fragments_and_original_expiry() {
    let (mut sender, mut receiver) = pair(MAX_PAYLOAD);
    let (Control::TransferStart(start), _) = sender
        .begin_outgoing(unit(), [4; 8], vec![1; 64], 1, 1, |_| Ok(()))
        .unwrap()
    else {
        panic!()
    };
    receiver
        .reject_start_admission(start, 1, 1, |_| Ok(()))
        .unwrap();
    assert_eq!(
        receiver.receive_peer(
            PeerFrame::LoroBatch {
                workspace_id: ws(),
                messages: vec![vec![0xff]]
            },
            2,
            |_| Ok(())
        ),
        Err(Error::Invalid)
    );
    let (mut sender, mut receiver) = pair(MAX_PAYLOAD);
    let (start, _) = sender
        .begin_outgoing(unit(), [4; 8], vec![1; 64], 1, 1, |_| Ok(()))
        .unwrap();
    receiver.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    receiver.reject_incoming_admission(2).unwrap();
    assert_eq!(receiver.status().incoming_bytes, 0);
    assert!(matches!(
        receiver.check(receiver.expires_at()),
        Err(Error::Authority(_))
    ));
}
