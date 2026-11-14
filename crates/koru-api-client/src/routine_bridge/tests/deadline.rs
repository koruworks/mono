// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_deadline_keeps_original_instant_and_clamps_long_callers() {
    let start = Instant::now();
    let original = start + Duration::from_millis(80);
    assert_eq!(
        bounded_deadline(original, start + Duration::from_millis(70)).unwrap(),
        original
    );
    assert_eq!(
        bounded_deadline(start + Duration::from_secs(30), start).unwrap(),
        start + ROUTINE_BRIDGE_DEADLINE
    );
    assert_eq!(
        bounded_deadline(start, start + Duration::from_secs(1)).unwrap(),
        start
    );
}

#[test]
fn routine_deadline_expired_overloads_are_not_sent_and_do_not_take_shared_slot() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let past = Instant::now();
    assert_eq!(
        b.read_preparation_until(&read(), past),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
    );
    assert_eq!(
        b.read_link_until(
            &RoutineBridgeEnvelope::new(ReadOccurrenceLink { claim: claim() }).unwrap(),
            past
        ),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
    );
    assert_eq!(
        b.finalize_until(
            &RoutineBridgeEnvelope::new(FinalizeOccurrence { claim: claim() }).unwrap(),
            past
        ),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
    );
    let preclaim = RoutineBridgeEnvelope::new(SynchronizeRoutinePreclaim {
        slot: slot(),
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
        preparation_hash: RoutineDigest32::from_bytes([2; 32]),
    })
    .unwrap();
    assert_eq!(
        b.synchronize_preclaim_until(&preclaim, past),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
    );
    assert!(!b.inner.in_flight.load(Ordering::Acquire));
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn routine_deadline_actual_http_reply_is_correlated_without_resetting_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, body) = request(&mut stream);
        let r = RoutineBridgeEnvelope::<ReadPreparation>::decode(&body).unwrap();
        answer(&mut stream, "200 OK", &missing(&r));
    });
    let r = b
        .read_preparation_until(&read(), Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(r.payload, ReadPreparationReply::Missing { .. }));
    server.join().unwrap();
}

#[test]
fn routine_deadline_physical_wait_retains_slot_until_unknown_call_returns() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let (seen, received) = mpsc::sync_channel(1);
    let (release, stop) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        seen.send(()).unwrap();
        stop.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    let other = b.clone();
    let call = std::thread::spawn(move || {
        other.read_preparation_until(&read(), Instant::now() + Duration::from_millis(400))
    });
    received.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        b.read_link_until(
            &RoutineBridgeEnvelope::new(ReadOccurrenceLink { claim: claim() }).unwrap(),
            Instant::now() + Duration::from_secs(1)
        ),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Capacity))
    );
    assert!(matches!(
        call.join().unwrap(),
        Err(RoutineBridgeTransportError::Unknown(_))
    ));
    assert!(!b.inner.in_flight.load(Ordering::Acquire));
    release.send(()).unwrap();
    server.join().unwrap();
}
