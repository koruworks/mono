// SPDX-License-Identifier: MPL-2.0
//! Hermetic loopback HTTP and in-memory body evidence, no Store/worker opens.
use super::*;
use koru_delegate_core::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
mod deadline;
mod preclaim;

fn id(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
}
fn slot() -> RoutineSlot {
    serde_json::from_value(serde_json::json!({"owner":{"user_scope":id(1),"device":id(2)},"bot":id(3),"schedule":id(4),"occurrence_ms":60_000})).unwrap()
}
fn read() -> RoutineBridgeEnvelope<ReadPreparation> {
    RoutineBridgeEnvelope::new(ReadPreparation {
        slot: slot(),
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
    })
    .unwrap()
}
fn claim() -> RoutineClaimIdentity {
    RoutineClaimIdentity {
        slot: slot(),
        definition: RoutineDefinitionIdentity {
            revision: 1,
            digest: RoutineDigest32::from_bytes([2; 32]),
            owner_handoff: None,
        },
        ids: RoutineExecutionIds {
            submission: id(5).parse().unwrap(),
            run: id(6).parse().unwrap(),
            chat: id(7).parse().unwrap(),
            attempt: id(8).parse().unwrap(),
            user_message: id(9).parse().unwrap(),
            assistant_message: id(10).parse().unwrap(),
        },
        preparation_hash: RoutineDigest32::from_bytes([3; 32]),
        claimed_at_ms: 60_001,
    }
}
fn authority() -> RoutineBridgeAuthority {
    koru_delegate_api::bots::BotDelegateAuthority::new("PRIVATE-BRIDGE-TRANSPORT-0123456789".into())
        .unwrap()
        .routine_bridge_authority()
}
fn gateway(endpoint: String) -> koru_api_discovery::GatewayHello {
    koru_api_discovery::GatewayHello {
        instance_id: "bridge-test".into(),
        supervisor_launch_id: None,
        gateway_version: "test".into(),
        segment: "test".into(),
        api_version: 1,
        api_endpoint: endpoint,
        capabilities: vec![],
        session: koru_api_discovery::SessionCredential::new("ORDINARY-BEARER-SENTINEL".into(), 0),
    }
}
fn client(endpoint: String) -> crate::Client {
    crate::Client::from_gateway(gateway(endpoint)).unwrap()
}
fn bridge(listener: &TcpListener) -> RoutineBridgeClient {
    client(format!("http://{}", listener.local_addr().unwrap()))
        .routine_bridge(authority())
        .unwrap()
}
fn request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        headers.push_str(&line);
        assert!(headers.len() < 16384);
        if line == "\r\n" {
            break;
        }
    }
    let length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length <= ROUTINE_BRIDGE_BODY_LIMIT);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let authorization = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim())
        })
        .unwrap();
    assert!(authority().matches_authorization(authorization.as_bytes()));
    assert!(!headers.contains("ORDINARY-BEARER-SENTINEL"));
    (headers, body)
}
fn answer(stream: &mut TcpStream, status: &str, body: &[u8]) {
    write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}
fn missing(request: &RoutineBridgeEnvelope<ReadPreparation>) -> Vec<u8> {
    RoutineBridgeEnvelope::reply_to(
        request,
        ReadPreparationReply::Missing {
            slot: request.payload.slot,
            semantic_hash: request.payload.semantic_hash,
        },
    )
    .unwrap()
    .encode()
    .unwrap()
}

#[test]
fn routine_transport_validates_raw_literal_loopback_origin_without_normalization_shortcuts() {
    for value in [
        "http://127.0.0.1:9",
        "http://127.1.2.3:65535/",
        "http://[::1]:80/",
        "http://127.0.0.1:80",
    ] {
        assert!(origin(value).is_ok(), "{value}")
    }
    for value in [
        "https://127.0.0.1:9",
        "http://localhost:9",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:9/path",
        "http://127.0.0.1:9//",
        "http://127.0.0.1:9?x",
        "http://127.0.0.1:9#x",
        "http://u:p@127.0.0.1:9",
        "http://2130706433:9",
        "http://127.1:9",
        "http://0177.0.0.1:9",
        "http://[::ffff:127.0.0.1]:9",
        "http://192.168.1.1:9",
        "http://0.0.0.0:9",
        "http://[::]:9",
    ] {
        assert_eq!(
            origin(value),
            Err(RoutineBridgeTransportError::NotSent(
                BeforeSend::InvalidEndpoint
            )),
            "{value}"
        )
    }
    // GatewayHello is not independently trusted. Revalidate its snapshot.
    let c = client("http://127.0.0.1:9".into());
    c.gateway
        .as_ref()
        .unwrap()
        .current
        .write()
        .unwrap()
        .api_endpoint = "http://localhost:9".into();
    assert!(matches!(
        c.routine_bridge(authority()),
        Err(RoutineBridgeTransportError::NotSent(
            BeforeSend::InvalidEndpoint
        ))
    ));
}

#[test]
fn routine_transport_snapshots_endpoint_without_bearer_renewal_and_redacts_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let c = crate::Client::from_gateway_with_renewal(
        gateway(endpoint),
        Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            panic!("private bridge must not renew ordinary session")
        }),
    )
    .unwrap();
    let b = c.routine_bridge(authority()).unwrap();
    c.gateway
        .as_ref()
        .unwrap()
        .current
        .write()
        .unwrap()
        .api_endpoint = "http://127.0.0.1:1".into();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (headers, body) = request(&mut stream);
        assert!(headers.starts_with("POST /internal/bots/routines/v1/preparation HTTP/1.1"));
        let request = RoutineBridgeEnvelope::<ReadPreparation>::decode(&body).unwrap();
        answer(&mut stream, "200 OK", &missing(&request));
    });
    let request = read();
    let reply = b.read_preparation(&request).unwrap();
    assert!(matches!(
        reply.payload,
        ReadPreparationReply::Missing { .. }
    ));
    server.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(b.inner.authorization.is_sensitive());
    let debug = format!("{b:?} {:?}", b.inner.authorization);
    assert!(!debug.contains(&authority().authorization_header()));
    assert!(!debug.contains("ORDINARY-BEARER-SENTINEL"));
}

#[test]
fn routine_transport_uses_all_four_committed_envelopes_and_exact_private_paths() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let server = std::thread::spawn(move || {
        for path in [
            ROUTINE_PREPARE_ROUTE,
            ROUTINE_PREPARATION_ROUTE,
            ROUTINE_FINALIZE_ROUTE,
            ROUTINE_LINK_ROUTE,
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let (headers, body) = request(&mut stream);
            assert!(headers.starts_with(&format!("POST {path} HTTP/1.1")));
            let reply = match path {
                ROUTINE_PREPARE_ROUTE => {
                    let r = RoutineBridgeEnvelope::<PrepareOccurrence>::decode(&body).unwrap();
                    RoutineBridgeEnvelope::reply_to(
                        &r,
                        PrepareOccurrenceReply::Closed {
                            slot: r.payload.slot,
                            semantic_hash: RoutineDigest32::from_bytes([1; 32]),
                            reason: BotRoutineCloseReason::ExpiredUnclaimed,
                        },
                    )
                    .unwrap()
                    .encode()
                    .unwrap()
                }
                ROUTINE_PREPARATION_ROUTE => {
                    missing(&RoutineBridgeEnvelope::<ReadPreparation>::decode(&body).unwrap())
                }
                ROUTINE_FINALIZE_ROUTE => {
                    let r = RoutineBridgeEnvelope::<FinalizeOccurrence>::decode(&body).unwrap();
                    RoutineBridgeEnvelope::reply_to(
                        &r,
                        FinalizeOccurrenceReply::Terminal {
                            claim: r.payload.claim.clone(),
                            reason: RoutineTerminalReason::AlreadyTerminal,
                        },
                    )
                    .unwrap()
                    .encode()
                    .unwrap()
                }
                _ => {
                    let r = RoutineBridgeEnvelope::<ReadOccurrenceLink>::decode(&body).unwrap();
                    RoutineBridgeEnvelope::reply_to(
                        &r,
                        ReadOccurrenceLinkReply::Missing {
                            claim: r.payload.claim.clone(),
                        },
                    )
                    .unwrap()
                    .encode()
                    .unwrap()
                }
            };
            answer(&mut stream, "200 OK", &reply);
        }
    });
    let payload:PrepareOccurrence=serde_json::from_value(serde_json::json!({"slot":slot(),"workspace":id(11),"owner_handoff":null,"schedule_definition":{"revision":1,"effective_at_ms":0,"template":{"requested_by":{"kind":"person","profile":id(12)},"objective":"private task","agent":{"kind":"built_in","value":"codex"},"requested_model":null,"scope":{"kind":"bot_folder","folder":"/fixture/bot"},"grants":[id(11)],"context":[]},"recurrence":{"type":"floating","cron":"*/1 * * * *"},"lifecycle":"active","routine":{"owner":slot().owner,"bot":slot().bot,"home":id(11),"creation_request":id(13)}}})).unwrap();
    assert!(matches!(
        b.prepare_until(
            &RoutineBridgeEnvelope::new(payload).unwrap(),
            Instant::now() + ROUTINE_BRIDGE_DEADLINE
        )
        .unwrap()
        .payload,
        PrepareOccurrenceReply::Closed { .. }
    ));
    assert!(matches!(
        b.read_preparation_until(&read(), Instant::now() + ROUTINE_BRIDGE_DEADLINE)
            .unwrap()
            .payload,
        ReadPreparationReply::Missing { .. }
    ));
    assert!(matches!(
        b.finalize_until(
            &RoutineBridgeEnvelope::new(FinalizeOccurrence { claim: claim() }).unwrap(),
            Instant::now() + ROUTINE_BRIDGE_DEADLINE
        )
        .unwrap()
        .payload,
        FinalizeOccurrenceReply::Terminal { .. }
    ));
    assert!(matches!(
        b.read_link_until(
            &RoutineBridgeEnvelope::new(ReadOccurrenceLink { claim: claim() }).unwrap(),
            Instant::now() + ROUTINE_BRIDGE_DEADLINE
        )
        .unwrap()
        .payload,
        ReadOccurrenceLinkReply::Missing { .. }
    ));
    server.join().unwrap();
}

#[test]
fn routine_transport_rejects_auth_redirect_status_and_connection_loss_without_second_attempt() {
    for (status, expected) in [
        ("401 Unauthorized", AfterSend::Unauthorized),
        ("503 Service Unavailable", AfterSend::UnexpectedStatus(503)),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = bridge(&listener);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&mut stream);
            answer(&mut stream, status, b"{}");
            listener
        });
        assert_eq!(
            b.read_preparation(&read()),
            Err(RoutineBridgeTransportError::Unknown(expected))
        );
        let listener = server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_url = format!("http://{}/redirected", target.local_addr().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        write!(stream,"HTTP/1.1 307 Temporary Redirect\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    assert_eq!(
        b.read_preparation(&read()),
        Err(RoutineBridgeTransportError::Unknown(AfterSend::Redirect))
    );
    server.join().unwrap();
    target.set_nonblocking(true).unwrap();
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        drop(stream);
        listener
    });
    assert_eq!(
        b.read_preparation(&read()),
        Err(RoutineBridgeTransportError::Unknown(AfterSend::Transport))
    );
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn routine_transport_preserves_unknown_for_invalid_or_mismatched_success_bodies() {
    for kind in ["malformed", "request_id", "slot", "hash"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = bridge(&listener);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = request(&mut stream);
            let r = RoutineBridgeEnvelope::<ReadPreparation>::decode(&body).unwrap();
            let mut response = serde_json::from_slice::<serde_json::Value>(&missing(&r)).unwrap();
            match kind {
                "request_id" => response["request_id"] = serde_json::json!(id(30)),
                "slot" => response["payload"]["slot"]["occurrence_ms"] = 123.into(),
                "hash" => response["payload"]["semantic_hash"] = "ab".repeat(32).into(),
                _ => {}
            }
            let body = if kind == "malformed" {
                b"{invalid".to_vec()
            } else {
                serde_json::to_vec(&response).unwrap()
            };
            answer(&mut stream, "200 OK", &body);
        });
        let expected = if kind == "malformed" {
            AfterSend::InvalidResponse
        } else {
            AfterSend::MismatchedResponse
        };
        assert_eq!(
            b.read_preparation(&read()),
            Err(RoutineBridgeTransportError::Unknown(expected))
        );
        server.join().unwrap();
    }
}

#[test]
fn routine_transport_enforces_finite_response_read_and_request_validation_before_io() {
    let deadline = Instant::now() + Duration::from_secs(5);
    let exact = vec![b'x'; ROUTINE_BRIDGE_BODY_LIMIT];
    assert_eq!(
        read_bounded(&mut &exact[..], None, deadline).unwrap().len(),
        ROUTINE_BRIDGE_BODY_LIMIT
    );
    struct CountRead {
        read: usize,
    }
    impl Read for CountRead {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.read += bytes.len();
            bytes.fill(b'x');
            Ok(bytes.len())
        }
    }
    let mut r = CountRead { read: 0 };
    assert_eq!(
        read_bounded(&mut r, None, deadline),
        Err(RoutineBridgeTransportError::Unknown(
            AfterSend::ContextTooLarge
        ))
    );
    assert_eq!(r.read, ROUTINE_BRIDGE_BODY_LIMIT + 1);
    struct NoRead;
    impl Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("declared oversize/expired must not read")
        }
    }
    assert!(matches!(
        read_bounded(
            &mut NoRead,
            Some((ROUTINE_BRIDGE_BODY_LIMIT + 1) as u64),
            deadline
        ),
        Err(RoutineBridgeTransportError::Unknown(
            AfterSend::ContextTooLarge
        ))
    ));
    assert_eq!(
        read_bounded(&mut NoRead, None, Instant::now()),
        Err(RoutineBridgeTransportError::Unknown(AfterSend::Deadline))
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let mut invalid = read();
    invalid.version = 2;
    assert_eq!(
        b.read_preparation(&invalid),
        Err(RoutineBridgeTransportError::NotSent(
            BeforeSend::InvalidRequest
        ))
    );
    assert_eq!(
        b.call::<_, ReadPreparationReply>(ROUTINE_PREPARATION_ROUTE, &read(), Duration::ZERO),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn routine_transport_clone_gate_is_nonblocking_and_covers_the_physical_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let other = b.clone();
    let (opened, wait_opened) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        for n in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = request(&mut stream);
            let r = RoutineBridgeEnvelope::<ReadPreparation>::decode(&body).unwrap();
            if n == 0 {
                opened.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            answer(&mut stream, "200 OK", &missing(&r));
        }
    });
    let worker = std::thread::spawn(move || other.read_preparation(&read()));
    wait_opened.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(b.inner.in_flight.load(Ordering::Acquire));
    assert_eq!(
        b.read_preparation(&read()),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Capacity))
    );
    assert!(!worker.is_finished());
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_ok());
    assert!(!b.inner.in_flight.load(Ordering::Acquire));
    assert!(b.read_preparation(&read()).is_ok());
    server.join().unwrap();
}

#[test]
fn routine_transport_body_deadline_returns_unknown_and_releases_only_on_call_return() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = bridge(&listener);
    let other = b.clone();
    let (opened, wait_opened) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{")
            .unwrap();
        stream.flush().unwrap();
        opened.send(()).unwrap();
        wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    let worker = std::thread::spawn(move || {
        other.call::<_, ReadPreparationReply>(
            ROUTINE_PREPARATION_ROUTE,
            &read(),
            Duration::from_millis(300),
        )
    });
    wait_opened.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(b.inner.in_flight.load(Ordering::Acquire));
    assert_eq!(
        b.read_preparation(&read()),
        Err(RoutineBridgeTransportError::NotSent(BeforeSend::Capacity))
    );
    assert_eq!(
        worker.join().unwrap(),
        Err(RoutineBridgeTransportError::Unknown(AfterSend::Deadline))
    );
    assert!(!b.inner.in_flight.load(Ordering::Acquire));
    release.send(()).unwrap();
    server.join().unwrap();
    assert_eq!(ROUTINE_BRIDGE_DEADLINE, Duration::from_secs(5));
}
