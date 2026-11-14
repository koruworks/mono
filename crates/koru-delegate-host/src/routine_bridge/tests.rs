// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub(crate) fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("bounded fixture accept: {error}"),
        }
    }
}

pub(crate) fn authority() -> BotDelegateAuthority {
    BotDelegateAuthority::new("HOST-PRIVATE-ROUTINE-0123456789012345".into()).unwrap()
}
pub(crate) fn gateway(origin: &str) -> koru_api_client::Client {
    koru_api_client::Client::from_gateway(
        serde_json::from_value(serde_json::json!({
            "instance_id":"fixture", "supervisor_launch_id":null,"gateway_version":"fixture",
            "segment":"fixture","api_version":1,"api_endpoint":origin,"capabilities":[],
            "session":{"bearer":"ORDINARY-BEARER-MUST-NOT-APPEAR","expires_at_millis":0}
        }))
        .unwrap(),
    )
    .unwrap()
}
fn slot() -> RoutineSlot {
    serde_json::from_value(serde_json::json!({"owner":{"user_scope":uuid::Uuid::new_v4(),"device":uuid::Uuid::new_v4()},
        "bot":uuid::Uuid::new_v4(),"schedule":uuid::Uuid::new_v4(),"occurrence_ms":60000})).unwrap()
}
pub(crate) fn receive(stream: &mut TcpStream) -> serde_json::Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
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
    assert!(!headers.contains("ORDINARY-BEARER-MUST-NOT-APPEAR"));
    let auth = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim())
        })
        .unwrap();
    assert!(
        authority()
            .routine_bridge_authority()
            .matches_authorization(auth.as_bytes())
    );
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse().unwrap())
        })
        .unwrap();
    assert!(length <= ROUTINE_BRIDGE_BODY_LIMIT);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}
pub(crate) fn send(stream: &mut TcpStream, status: &str, body: &[u8]) {
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

#[test]
fn routine_host_bridge_uses_only_literal_loopback_and_derived_credential() {
    let stopped = Arc::new(AtomicBool::new(false));
    for origin in [
        "http://localhost:9",
        "https://127.0.0.1:9",
        "http://127.0.0.1:9/path",
    ] {
        let bridge = HostRoutineBridge::new(&gateway(origin), Some(&authority()), stopped.clone());
        assert!(bridge.client.is_none());
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut bridge = HostRoutineBridge::new(&gateway(&origin), Some(&authority()), stopped);
    let task = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let request = receive(&mut stream);
        let payload = serde_json::json!({"kind":"missing","slot":request["payload"]["slot"],"semantic_hash":request["payload"]["semantic_hash"]});
        let reply =
            serde_json::json!({"version":1,"request_id":request["request_id"],"payload":payload});
        send(&mut stream, "200 OK", &serde_json::to_vec(&reply).unwrap());
    });
    assert!(matches!(
        bridge
            .read_preparation(
                &slot(),
                RoutineDigest32::from_bytes([1; 32]),
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap(),
        RoutinePreparationObservation::Missing
    ));
    task.join().unwrap();
}

#[test]
fn routine_host_bridge_old_404_and_wrong_correlation_remain_unknown() {
    for wrong in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut bridge = HostRoutineBridge::new(
            &gateway(&format!("http://{}", listener.local_addr().unwrap())),
            Some(&authority()),
            Arc::new(AtomicBool::new(false)),
        );
        let task = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let _ = receive(&mut stream);
            if wrong {
                let reply = serde_json::json!({"version":1,"request_id":uuid::Uuid::new_v4(),"payload":{"kind":"refused","code":"unavailable"}});
                send(&mut stream, "200 OK", &serde_json::to_vec(&reply).unwrap());
            } else {
                send(&mut stream, "404 Not Found", b"{}");
            }
        });
        assert!(matches!(
            bridge.read_preparation(
                &slot(),
                RoutineDigest32::from_bytes([1; 32]),
                Instant::now() + Duration::from_secs(2)
            ),
            Err(RoutineBridgeFailure::Unknown(_))
        ));
        task.join().unwrap();
    }
}

#[test]
fn routine_host_bridge_stop_and_original_deadline_block_subsequent_phases() {
    let stopped = Arc::new(AtomicBool::new(true));
    let mut unavailable = HostRoutineBridge {
        client: None,
        stopped: stopped.clone(),
    };
    assert!(matches!(
        unavailable.read_preparation(
            &slot(),
            RoutineDigest32::from_bytes([1; 32]),
            Instant::now() + Duration::from_secs(1)
        ),
        Err(RoutineBridgeFailure::NotSent(_))
    ));
    stopped.store(false, Ordering::Release);
    assert!(matches!(
        unavailable.read_preparation(
            &slot(),
            RoutineDigest32::from_bytes([1; 32]),
            Instant::now()
        ),
        Err(RoutineBridgeFailure::NotSent(_))
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut bridge = HostRoutineBridge::new(
        &gateway(&format!("http://{}", listener.local_addr().unwrap())),
        Some(&authority()),
        stopped.clone(),
    );
    let task = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let request = receive(&mut stream);
        stopped.store(true, Ordering::Release);
        let reply = serde_json::json!({"version":1,"request_id":request["request_id"],"payload":{"kind":"refused","code":"unavailable"}});
        send(&mut stream, "200 OK", &serde_json::to_vec(&reply).unwrap());
    });
    assert!(matches!(
        bridge.read_preparation(
            &slot(),
            RoutineDigest32::from_bytes([1; 32]),
            Instant::now() + Duration::from_secs(2)
        ),
        Err(RoutineBridgeFailure::Unknown(_))
    ));
    task.join().unwrap();
}

#[test]
fn routine_host_bridge_all_five_routes_preserve_typed_refusal() {
    let f = crate::actor::tests::routine::Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut bridge = HostRoutineBridge::new(
        &gateway(&format!("http://{}", listener.local_addr().unwrap())),
        Some(&authority()),
        Arc::new(AtomicBool::new(false)),
    );
    let task = std::thread::spawn(move || {
        for _ in 0..5 {
            let mut stream = accept(&listener);
            let request = receive(&mut stream);
            let reply = serde_json::json!({"version":1,"request_id":request["request_id"],"payload":{"kind":"refused","code":"unavailable"}});
            send(&mut stream, "200 OK", &serde_json::to_vec(&reply).unwrap());
        }
    });
    let p = &f.prepared;
    let definition = ScheduleDefinition {
        revision: 1,
        effective_at_ms: 0,
        template: koru_delegate_schedule::DelegationTemplate {
            requested_by: p.submission.request.requested_by.clone(),
            objective: p.submission.request.objective.clone(),
            agent: p.submission.request.agent.clone(),
            requested_model: None,
            scope: p.submission.request.scope.clone(),
            grants: [p.submission.workspace].into_iter().collect(),
            context: vec![],
        },
        recurrence: koru_delegate_schedule::RecurrenceRule::floating("* * * * *".parse().unwrap()),
        lifecycle: koru_delegate_schedule::ScheduleLifecycle::Active,
        routine: Some(
            koru_delegate_schedule::RoutineAssociation::new(
                p.slot.owner,
                p.slot.bot,
                p.submission.workspace,
                BotRequestId::mint(),
            )
            .unwrap(),
        ),
    };
    let claim = RoutineClaimIdentity {
        slot: p.slot,
        definition: p.definition,
        ids: p.ids,
        preparation_hash: p.preparation_hash,
        claimed_at_ms: 1000,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(matches!(
        bridge.prepare(&p.slot, p.submission.workspace, &definition, None, deadline),
        Err(RoutineBridgeFailure::Refused(_))
    ));
    assert!(matches!(
        bridge.read_preparation(&p.slot, p.semantic_hash, deadline),
        Err(RoutineBridgeFailure::Refused(_))
    ));
    assert!(matches!(
        bridge.synchronize_preclaim(p, deadline),
        Err(RoutineBridgeFailure::Refused(_))
    ));
    assert!(matches!(
        bridge.read_link(&claim, deadline),
        Err(RoutineBridgeFailure::Refused(_))
    ));
    assert!(matches!(
        bridge.finalize(&claim, deadline),
        Err(RoutineBridgeFailure::Refused(_))
    ));
    task.join().unwrap();
}

#[test]
fn routine_host_bridge_positive_preparation_with_changed_definition_digest_is_unknown() {
    let f = crate::actor::tests::routine::Fixture::new();
    let mut p = f.prepared.clone();
    let definition = ScheduleDefinition {
        revision: 1,
        effective_at_ms: 0,
        template: koru_delegate_schedule::DelegationTemplate {
            requested_by: p.submission.request.requested_by.clone(),
            objective: p.submission.request.objective.clone(),
            agent: p.submission.request.agent.clone(),
            requested_model: None,
            scope: p.submission.request.scope.clone(),
            grants: [p.submission.workspace].into_iter().collect(),
            context: vec![],
        },
        recurrence: koru_delegate_schedule::RecurrenceRule::floating("* * * * *".parse().unwrap()),
        lifecycle: koru_delegate_schedule::ScheduleLifecycle::Active,
        routine: Some(
            koru_delegate_schedule::RoutineAssociation::new(
                p.slot.owner,
                p.slot.bot,
                p.submission.workspace,
                BotRequestId::mint(),
            )
            .unwrap(),
        ),
    };
    p.semantic_hash =
        routine_semantic_digest(&p.slot, &definition, p.submission.workspace, None).unwrap();
    assert_ne!(
        p.definition.digest,
        routine_definition_digest(&definition, p.submission.workspace, None).unwrap()
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut bridge = HostRoutineBridge::new(
        &gateway(&format!("http://{}", listener.local_addr().unwrap())),
        Some(&authority()),
        Arc::new(AtomicBool::new(false)),
    );
    let slot = p.slot;
    let workspace = p.submission.workspace;
    let task = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let request = receive(&mut stream);
        let reply = serde_json::json!({"version":1,"request_id":request["request_id"],"payload":{"kind":"prepared","preparation":p}});
        send(&mut stream, "200 OK", &serde_json::to_vec(&reply).unwrap());
    });
    assert!(matches!(
        bridge.prepare(
            &slot,
            workspace,
            &definition,
            None,
            Instant::now() + Duration::from_secs(2)
        ),
        Err(RoutineBridgeFailure::Unknown(_))
    ));
    task.join().unwrap();
}
