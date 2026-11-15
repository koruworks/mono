// SPDX-License-Identifier: MPL-2.0
//! Hermetic loopback HTTP only; no Store, worker or provider opens.
use super::*;
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, BotWaitCleanupIntentV1, BotWaitId, BotWaitRefusal, DecisionId,
    DecisionRevision, DecisionVersionV1,
};
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, BotId, BotOwner, RunAttemptId, RunId, SessionId,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn parent() -> koru_delegate_api::bots::BotDelegateAuthority {
    koru_delegate_api::bots::BotDelegateAuthority::new(
        "PRIVATE-DECISION-TRANSPORT-0123456789".into(),
    )
    .unwrap()
}
/// A fresh canonical v4 identity; this crate does not link `uuid` directly.
fn fresh() -> String {
    RunId::mint().as_uuid().to_string()
}
fn enroll() -> DecisionBridgeEnvelope<EnrollBotWaitCleanup> {
    let owner: BotOwner =
        serde_json::from_value(serde_json::json!({"user_scope":fresh(),"device":fresh()})).unwrap();
    let intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner,
        bot: BotId::mint(),
        run: RunId::mint(),
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(3).unwrap(),
        wait_revision: DecisionRevision::new(2).unwrap(),
        origin_attempt: RunAttemptId::mint(),
        origin_generation: AttemptGeneration::from_raw(1).unwrap(),
        origin_session: SessionId::mint(),
        origin_turn: AgentTurnId::mint(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    DecisionBridgeEnvelope::new(EnrollBotWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
    })
    .unwrap()
}
fn bridge(listener: &TcpListener) -> DecisionBridgeClient {
    crate::Client::from_gateway(koru_api_discovery::GatewayHello {
        instance_id: "decision-bridge-test".into(),
        supervisor_launch_id: None,
        gateway_version: "test".into(),
        segment: "test".into(),
        api_version: 1,
        api_endpoint: format!("http://{}", listener.local_addr().unwrap()),
        capabilities: vec![],
        session: koru_api_discovery::SessionCredential::new("ORDINARY-BEARER-SENTINEL".into(), 0),
    })
    .unwrap()
    .decision_bridge(parent().decision_bridge_authority())
    .unwrap()
}
/// Read one request; prove the decision credential (and not the Routine one
/// or the ordinary bearer) authorized it, at the contract's literal path.
fn request(stream: &mut TcpStream) -> Vec<u8> {
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
    assert!(headers.starts_with(&format!("POST {DECISION_ENROLL_ROUTE} ")));
    let header = |name: &str| {
        headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.trim().to_owned())
            })
            .unwrap()
    };
    let authorization = header("authorization");
    assert!(
        parent()
            .decision_bridge_authority()
            .matches_authorization(authorization.as_bytes())
    );
    assert!(
        !parent()
            .routine_bridge_authority()
            .matches_authorization(authorization.as_bytes())
    );
    assert!(!headers.contains("ORDINARY-BEARER-SENTINEL"));
    let mut body = vec![0; header("content-length").parse().unwrap()];
    reader.read_exact(&mut body).unwrap();
    body
}
fn answer(stream: &mut TcpStream, status: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}
fn serve(listener: TcpListener, reply: impl FnOnce(&mut TcpStream, Vec<u8>) + Send + 'static) {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let body = request(&mut stream);
        reply(&mut stream, body);
    });
}
fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn decision_transport_returns_only_the_exact_correlated_enrolled_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = bridge(&listener);
    let request = enroll();
    let expected = request.clone();
    serve(listener, move |stream, body| {
        let received = DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(&body).unwrap();
        assert_eq!(received, expected);
        let key = received.payload.key.clone();
        let reply = DecisionBridgeEnvelope::reply_to(
            &received,
            BotWaitEnrollmentReply::Enrolled {
                receipt_digest: key.receipt_digest,
                key,
                ai_attempt: RunId::mint().as_uuid(),
            },
        )
        .unwrap();
        answer(stream, "200 OK", &reply.encode().unwrap());
    });
    let reply = client.enroll_until(&request, soon()).unwrap();
    assert!(matches!(
        reply.payload,
        BotWaitEnrollmentReply::Enrolled { .. }
    ));
}

#[test]
fn decision_transport_maps_every_post_send_failure_to_unknown_never_missing() {
    type Reply = fn(&mut TcpStream, Vec<u8>);
    let cases: [(Reply, AfterSend); 5] = [
        // Lost response: the request arrived; the connection closed unanswered.
        (|_, _| {}, AfterSend::Transport),
        (
            |stream, _| answer(stream, "401 Unauthorized", b"{}"),
            AfterSend::Unauthorized,
        ),
        (
            |stream, _| answer(stream, "504 Gateway Timeout", b"{}"),
            AfterSend::UnexpectedStatus(504),
        ),
        // Enroll may never answer Missing, even for the exact key.
        (
            |stream, body| {
                let received =
                    DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(&body).unwrap();
                let missing = DecisionBridgeEnvelope {
                    version: 1,
                    request_id: received.request_id,
                    payload: BotWaitEnrollmentReply::Missing {
                        key: received.payload.key.clone(),
                    },
                };
                answer(stream, "200 OK", &serde_json::to_vec(&missing).unwrap());
            },
            AfterSend::MismatchedResponse,
        ),
        (
            |stream, _| answer(stream, "200 OK", b"{\"version\":1}"),
            AfterSend::InvalidResponse,
        ),
    ];
    for (reply, expected) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = bridge(&listener);
        serve(listener, reply);
        assert_eq!(
            client.enroll_until(&enroll(), soon()).unwrap_err(),
            DecisionBridgeTransportError::Unknown(expected)
        );
    }
}

#[test]
fn decision_transport_timeout_is_unknown_and_refusal_is_a_typed_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = bridge(&listener);
    let (release, wait) = std::sync::mpsc::channel::<()>();
    serve(listener, move |_stream, _| {
        // Hold the connection open past the caller's deadline.
        let _ = wait.recv_timeout(Duration::from_secs(3));
    });
    let started = Instant::now();
    assert_eq!(
        client
            .enroll_until(&enroll(), Instant::now() + Duration::from_millis(300))
            .unwrap_err(),
        DecisionBridgeTransportError::Unknown(AfterSend::Deadline),
        "a timeout is never Missing"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let _ = release.send(());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = bridge(&listener);
    serve(listener, |stream, body| {
        let received = DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(&body).unwrap();
        let reply = DecisionBridgeEnvelope::reply_to(
            &received,
            BotWaitEnrollmentReply::Refused {
                code: BotWaitRefusal::Stale,
            },
        )
        .unwrap();
        answer(stream, "200 OK", &reply.encode().unwrap());
    });
    assert!(matches!(
        client.enroll_until(&enroll(), soon()).unwrap().payload,
        BotWaitEnrollmentReply::Refused {
            code: BotWaitRefusal::Stale
        }
    ));
    // Nothing is sent once the caller's deadline has passed.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = bridge(&listener);
    assert_eq!(
        client.enroll_until(&enroll(), Instant::now()).unwrap_err(),
        DecisionBridgeTransportError::NotSent(BeforeSend::Deadline)
    );
}
