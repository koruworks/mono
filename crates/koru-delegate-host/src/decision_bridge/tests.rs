// SPDX-License-Identifier: MPL-2.0
//! Hermetic loopback HTTP only; no Store, AI worker or provider opens.
use super::*;
use crate::routine_bridge::tests::{accept, authority, gateway};
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, BotWaitCleanupIntentV1, BotWaitId, DecisionId, DecisionRevision,
    DecisionVersionV1,
};
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, BotId, BotOwner, RunAttemptId, RunId, SessionId,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn request() -> EnrollBotWaitCleanup {
    let owner: BotOwner = serde_json::from_value(
        serde_json::json!({"user_scope":uuid::Uuid::new_v4(),"device":uuid::Uuid::new_v4()}),
    )
    .unwrap();
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
    EnrollBotWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
    }
}

/// Read the one request body after proving the decision credential.
fn body(stream: &mut TcpStream) -> Vec<u8> {
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
    let field = |name: &str| {
        headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.trim().to_owned())
            })
            .unwrap()
    };
    let credential = field("authorization");
    assert!(
        authority()
            .decision_bridge_authority()
            .matches_authorization(credential.as_bytes())
    );
    assert!(
        !authority()
            .routine_bridge_authority()
            .matches_authorization(credential.as_bytes())
    );
    let mut body = vec![0; field("content-length").parse().unwrap()];
    reader.read_exact(&mut body).unwrap();
    body
}

fn reply(
    stream: &mut TcpStream,
    payload: impl FnOnce(&EnrollBotWaitCleanup) -> BotWaitEnrollmentReply,
) {
    let received = DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(&body(stream)).unwrap();
    let answer = DecisionBridgeEnvelope::reply_to(&received, payload(&received.payload))
        .unwrap()
        .encode()
        .unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        answer.len()
    )
    .unwrap();
    stream.write_all(&answer).unwrap();
}

fn bridge(listener: &TcpListener) -> HostDecisionBridge {
    HostDecisionBridge::new(
        &gateway(&format!("http://{}", listener.local_addr().unwrap())),
        Some(&authority()),
        Arc::new(AtomicBool::new(false)),
    )
}

#[test]
fn host_decision_bridge_returns_only_an_exact_enrolled_receipt() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut bridge = bridge(&listener);
    let server = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        reply(&mut stream, |request| BotWaitEnrollmentReply::Enrolled {
            receipt_digest: request.key.receipt_digest,
            key: request.key.clone(),
            ai_attempt: uuid::Uuid::new_v4(),
        });
    });
    let request = request();
    let reply = bridge.enroll(&request).unwrap();
    assert!(reply.matches_request(&request.key, false));
    server.join().unwrap();
}

#[test]
fn host_decision_bridge_maps_refusal_lost_response_and_absence_without_missing() {
    // A typed refusal stays a refusal.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut refused = bridge(&listener);
    let server = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        reply(&mut stream, |_| BotWaitEnrollmentReply::Refused {
            code: BotWaitRefusal::Stale,
        });
    });
    assert_eq!(
        refused.enroll(&request()).unwrap_err().code,
        "decision_enrollment_stale"
    );
    server.join().unwrap();

    // The request arrived and the reply was lost: unknown, never Missing.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut lost = bridge(&listener);
    let server = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let _ = body(&mut stream);
    });
    assert_eq!(
        lost.enroll(&request()).unwrap_err().code,
        "decision_enrollment_unknown"
    );
    server.join().unwrap();

    // Without the capability nothing is sent and nothing borrows a bearer.
    assert_eq!(
        HostDecisionBridge::with_client(None)
            .enroll(&request())
            .unwrap_err()
            .code,
        "decision_bridge_unavailable"
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stopped = Arc::new(AtomicBool::new(true));
    let mut after_stop = HostDecisionBridge::new(
        &gateway(&format!("http://{}", listener.local_addr().unwrap())),
        Some(&authority()),
        stopped,
    );
    assert_eq!(
        after_stop.enroll(&request()).unwrap_err().code,
        "decision_enrollment_not_sent"
    );
}
