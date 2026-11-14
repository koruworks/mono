// SPDX-License-Identifier: MPL-2.0
use super::*;

fn preclaim() -> RoutineBridgeEnvelope<SynchronizeRoutinePreclaim> {
    RoutineBridgeEnvelope::new(SynchronizeRoutinePreclaim {
        slot: slot(),
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
        preparation_hash: RoutineDigest32::from_bytes([2; 32]),
    })
    .unwrap()
}
fn ready(request: &RoutineBridgeEnvelope<SynchronizeRoutinePreclaim>) -> Vec<u8> {
    let slot = request.payload.slot;
    let policy = serde_json::json!({"workspace":id(11),"destination":id(11),"access":"default","selected_pages":[]});
    let reply: SynchronizeRoutinePreclaimReply = serde_json::from_value(serde_json::json!({
        "kind":"ready","slot":slot,"semantic_hash":request.payload.semantic_hash,
        "preparation_hash":request.payload.preparation_hash,
        "admission":{"owner":slot.owner,"bot":slot.bot,"request":id(12),"epoch":2,"paused":false},
        "proof":{"global":{"key":{"owner":slot.owner,"bot":slot.bot,"request":id(13)},
            "config_revision":2,"policy_revision":1,"policy":policy,"effects_allowed":true},
        "routine":{"owner":slot.owner,"bot":slot.bot,"schedule":slot.schedule,"request":id(14),
            "grant_revision":1,"policy":policy,"effects_allowed":true}}
    }))
    .unwrap();
    RoutineBridgeEnvelope::reply_to(request, reply)
        .unwrap()
        .encode()
        .unwrap()
}

#[test]
fn preclaim_transport_posts_exact_route_and_validates_current_ready_identity() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let sent = preclaim();
    let expected = sent.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (headers, body) = request(&mut stream);
        assert!(headers.starts_with(&format!("POST {ROUTINE_PRECLAIM_ROUTE} HTTP/1.1\r\n")));
        let decoded = RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(&body).unwrap();
        assert_eq!(decoded, expected);
        answer(&mut stream, "200 OK", &ready(&decoded));
    });
    let reply = b
        .synchronize_preclaim_until(&sent, Instant::now() + ROUTINE_BRIDGE_DEADLINE)
        .unwrap();
    assert_eq!(reply.encode().unwrap(), ready(&sent));
    assert!(matches!(
        reply.payload,
        SynchronizeRoutinePreclaimReply::Ready { .. }
    ));
    server.join().unwrap();
}

#[test]
fn preclaim_transport_old_endpoint_redirect_and_loss_remain_unknown_without_retry() {
    for kind in ["old", "redirect", "loss"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_url = format!("http://{}/unexpected", target.local_addr().unwrap());
        let b = bridge(&listener);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&mut stream);
            match kind {
                "old"=>answer(&mut stream,"404 Not Found",b"{}"),
                "redirect"=>write!(stream,"HTTP/1.1 307 Temporary Redirect\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap(),
                _=>{},
            }
            drop(stream);
            listener
        });
        let expected = match kind {
            "old" => AfterSend::UnexpectedStatus(404),
            "redirect" => AfterSend::Redirect,
            _ => AfterSend::Transport,
        };
        assert_eq!(
            b.synchronize_preclaim(&preclaim()),
            Err(RoutineBridgeTransportError::Unknown(expected))
        );
        for listener in [server.join().unwrap(), target] {
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
}

#[test]
fn preclaim_transport_malformed_proof_and_correlated_mismatches_never_become_ready() {
    for kind in [
        "proof",
        "paused",
        "unknown",
        "request",
        "semantic",
        "preparation",
        "slot",
        "oversize",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = bridge(&listener);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = request(&mut stream);
            let decoded =
                RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(&body).unwrap();
            let mut value: serde_json::Value = serde_json::from_slice(&ready(&decoded)).unwrap();
            match kind {
                "proof" => value["payload"]["proof"]["routine"]["schedule"] = id(40).into(),
                "paused" => value["payload"]["admission"]["paused"] = true.into(),
                "unknown" => value["payload"]["proof"]["global"]["unknown"] = true.into(),
                "request" => value["request_id"] = id(40).into(),
                "semantic" => value["payload"]["semantic_hash"] = "ab".repeat(32).into(),
                "preparation" => value["payload"]["preparation_hash"] = "ab".repeat(32).into(),
                "slot" => value["payload"]["slot"]["occurrence_ms"] = 123.into(),
                "oversize" => {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        ROUTINE_BRIDGE_BODY_LIMIT + 1
                    )
                    .unwrap();
                    return;
                }
                _ => unreachable!(),
            }
            answer(&mut stream, "200 OK", &serde_json::to_vec(&value).unwrap());
        });
        let expected = match kind {
            "proof" | "paused" | "unknown" => AfterSend::InvalidResponse,
            "oversize" => AfterSend::ContextTooLarge,
            _ => AfterSend::MismatchedResponse,
        };
        assert_eq!(
            b.synchronize_preclaim(&preclaim()),
            Err(RoutineBridgeTransportError::Unknown(expected)),
            "{kind}"
        );
        server.join().unwrap();
    }
}

#[test]
fn preclaim_transport_shares_physical_call_slot_with_existing_routes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let other = b.clone();
    let (opened, wait_opened) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, body) = request(&mut stream);
        let decoded = RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(&body).unwrap();
        opened.send(()).unwrap();
        wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
        answer(&mut stream, "200 OK", &ready(&decoded));
    });
    let worker = std::thread::spawn(move || other.synchronize_preclaim(&preclaim()));
    wait_opened.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(b.inner.in_flight.load(Ordering::Acquire));
    assert_eq!(
        b.synchronize_preclaim(&preclaim()),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Capacity))
    );
    assert_eq!(
        b.read_preparation(&read()),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Capacity))
    );
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_ok());
    server.join().unwrap();
    assert!(!b.inner.in_flight.load(Ordering::Acquire));
}

#[test]
fn preclaim_transport_deadline_and_invalid_request_preserve_finite_call_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let mut invalid = preclaim();
    invalid.version = 2;
    assert_eq!(
        b.synchronize_preclaim(&invalid),
        Err(RoutineBridgeTransportError::NotSent(
            BeforeSend::InvalidRequest
        ))
    );
    assert_eq!(
        b.call::<_, SynchronizeRoutinePreclaimReply>(
            ROUTINE_PRECLAIM_ROUTE,
            &preclaim(),
            Duration::ZERO
        ),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
    );
    let (release, wait_release) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, body) = request(&mut stream);
        assert!(RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(&body).is_ok());
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{")
            .unwrap();
        stream.flush().unwrap();
        wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    assert_eq!(
        b.call::<_, SynchronizeRoutinePreclaimReply>(
            ROUTINE_PRECLAIM_ROUTE,
            &preclaim(),
            Duration::from_millis(300)
        ),
        Err(RoutineBridgeTransportError::Unknown(AfterSend::Deadline))
    );
    assert!(!b.inner.in_flight.load(Ordering::Acquire));
    release.send(()).unwrap();
    server.join().unwrap();
}
