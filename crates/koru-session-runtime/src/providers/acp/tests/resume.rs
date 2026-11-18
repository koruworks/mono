// SPDX-License-Identifier: MPL-2.0

use super::*;

fn qualified(replay: Vec<Value>, answer: Value) -> Harness {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.route = AcpRoute::claude_code();
    h.control.config.executable_version = "0.76.0".parse().unwrap();
    h.control.config.delegation = Some(delegation());
    let mut frames = h.injected.lock().unwrap();
    frames.push_back(
        json!({"jsonrpc":"2.0","method":"_auth/status_update","params":{
            "authStatus":{"kind":"account"}
        }}),
    );
    frames.push_back(response(
        1,
        json!({"protocolVersion":1,
            "agentInfo":{"name":"<wrapper>","version":"0.76.0"},
            "agentCapabilities":{"loadSession":true,"_meta":{"authStatus":{}},
                "mcpCapabilities":{"http":true}}
        }),
    ));
    frames.extend(replay);
    frames.push_back(answer);
    drop(frames);
    h
}

fn saved() -> ProviderResumeToken {
    json!({"wrapper":"0.76.0","session":SESSION})
        .to_string()
        .parse()
        .unwrap()
}

fn load(h: &mut Harness) -> Result<AgentCapabilities, ProviderFailure> {
    h.control.start(AgentStart {
        session_id: SessionId::mint(),
        parent_session_id: None,
        resume: Some(saved()),
    })
}

fn update(kind: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":SESSION,"update":{"sessionUpdate":kind,
            "content":{"type":"text","text":text}}
    }})
}

#[test]
fn native_load_rebinds_credentials_and_replay_is_not_live_activity() {
    let mut h = qualified(
        vec![
            update("user_message_chunk", "exact input"),
            update(
                "agent_message_chunk",
                "old answer containing another delivery marker",
            ),
            json!({"jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":SESSION,"update":{"sessionUpdate":"tool_call","toolCallId":"old",
                    "title":"old tool", "kind":"other","status":"completed"}
            }}),
            json!({"jsonrpc":"2.0","id":"old-request","method":"session/request_permission",
            "params":{"sessionId":SESSION}}),
        ],
        response(2, json!({})),
    );
    let caps = load(&mut h).unwrap();
    assert_eq!(caps.delegation.resume, ResumeSupport::Native);
    assert_eq!(
        h.control.replayed_input_hashes(),
        &[*blake3::hash(b"exact input").as_bytes()]
    );
    assert!(
        h.control
            .events
            .iter()
            .all(|event| matches!(event, AgentEvent::ProviderState { .. }))
    );
    assert!(h.control.unsaid_notices.is_empty());
    assert!(h.control.tools.is_empty());
    let token = h.control.resume_token().unwrap();
    assert!(!token.as_str().contains("secret-bearer-test"));
    assert!(!format!("{token:?}").contains(SESSION));
    let state = h.state.lock().unwrap();
    let sent = state.sent();
    assert!(!sent.iter().any(|frame| frame["method"] == "session/new"));
    let params = &sent
        .iter()
        .find(|frame| frame["method"] == "session/load")
        .unwrap()["params"];
    assert_eq!(params["sessionId"], SESSION);
    assert_eq!(
        params["mcpServers"][0]["headers"][0]["value"],
        "Bearer secret-bearer-test"
    );
    assert!(
        sent.iter()
            .any(|frame| frame["id"] == "old-request" && frame["error"]["code"] == -32601)
    );
}

#[test]
fn failed_load_has_no_evidence_and_cannot_restart_in_the_same_process() {
    let mut h = qualified(
        vec![update("user_message_chunk", "partial history")],
        json!({"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"PRIVATE_FAILURE"}}),
    );
    let failure = load(&mut h).unwrap_err();
    assert!(!format!("{failure:?}").contains("PRIVATE_FAILURE"));
    assert!(h.control.replayed_input_hashes().is_empty());
    assert!(h.control.resume_token().is_none());
    assert!(
        h.control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .is_err()
    );
}

#[test]
fn only_qualified_advertised_route_mints_or_accepts_tokens() {
    for route in [
        AcpRoute::opencode(),
        AcpRoute::kimi(),
        AcpRoute::grok(),
        AcpRoute::antigravity(),
        AcpRoute::devin(),
    ] {
        let mut h = qualified(vec![], response(2, json!({})));
        h.control.config.route = route;
        assert!(load(&mut h).is_err());
        assert!(h.state.lock().unwrap().sent().is_empty());
        assert!(h.control.resume_token().is_none());
    }
    let mut h = qualified(vec![], response(2, json!({})));
    h.control.config.executable_version = "0.77.0".parse().unwrap();
    assert!(load(&mut h).is_err());
    assert!(h.state.lock().unwrap().sent().is_empty());

    let mut h = qualified(vec![], response(2, json!({})));
    for frame in h.injected.lock().unwrap().iter_mut() {
        if frame["id"] == 1 {
            frame["result"]["agentCapabilities"]["loadSession"] = json!(false);
        }
    }
    assert!(load(&mut h).is_err());
    assert!(
        !h.state
            .lock()
            .unwrap()
            .sent()
            .iter()
            .any(|f| f["method"] == "session/load")
    );

    let mut fresh = qualified(vec![], response(2, json!({"sessionId":SESSION})));
    start(&mut fresh.control);
    assert_eq!(fresh.control.resume_token().unwrap(), saved());
}

#[test]
fn foreign_and_split_replay_cannot_prove_a_complete_input() {
    let mut foreign = update("user_message_chunk", "whole input");
    foreign["params"]["sessionId"] = json!("foreign");
    let mut h = qualified(
        vec![
            foreign,
            update("user_message_chunk", "whole "),
            update("user_message_chunk", "input"),
            update("agent_message_chunk", "whole input"),
        ],
        response(2, json!({})),
    );
    load(&mut h).unwrap();
    assert!(
        !h.control
            .replayed_input_hashes()
            .contains(blake3::hash(b"whole input").as_bytes())
    );
    assert_eq!(h.control.replayed_input_hashes().len(), 2);
}
