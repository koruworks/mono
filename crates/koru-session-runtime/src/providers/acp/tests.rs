// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::NoopAttachmentPathResolver;
use crate::providers::acp_fake::{
    FakeAcpConfig, FakeAcpPeer, FakeAcpState, PERMISSION_ID, SESSION, late_update,
};
use koru_session_protocol::{LaunchSelection, ProviderKind};
use std::sync::{Arc, Mutex};

struct Script {
    peer: FakeAcpPeer,
    injected: Arc<Mutex<VecDeque<Value>>>,
    waits: Arc<Mutex<Vec<Duration>>>,
    initialize: Option<Value>,
    new_session: Option<Value>,
}

impl JsonRpcTransport for Script {
    fn send(&mut self, value: &Value) -> Result<(), ProviderFailure> {
        self.peer.send(value)
    }
    fn receive(&mut self, timeout: Duration) -> Result<TransportReceive, ProviderFailure> {
        self.waits.lock().unwrap().push(timeout);
        if let Some(frame) = self.injected.lock().unwrap().pop_front() {
            return Ok(TransportReceive::Frame(frame));
        }
        let received = self.peer.receive(timeout)?;
        if let TransportReceive::Frame(mut frame) = received {
            match frame["id"].as_u64() {
                Some(1) => {
                    if let Some(value) = self.initialize.take() {
                        frame = value;
                    }
                }
                Some(2) => {
                    if let Some(value) = self.new_session.take() {
                        frame = value;
                    }
                }
                _ => {}
            }
            Ok(TransportReceive::Frame(frame))
        } else {
            Ok(received)
        }
    }
    fn shutdown(&mut self, timeout: Duration) -> Result<(), ProviderFailure> {
        self.peer.shutdown(timeout)
    }
    fn terminate_tree(&mut self) -> Result<(), ProviderFailure> {
        self.peer.terminate_tree()
    }
}

struct Harness {
    control: AcpControl,
    state: Arc<Mutex<FakeAcpState>>,
    injected: Arc<Mutex<VecDeque<Value>>>,
    waits: Arc<Mutex<Vec<Duration>>>,
}

fn config() -> AcpConfig {
    let version = ProviderVersion::from_str("1.0.0").unwrap();
    AcpConfig::new(
        AcpRoute::new(
            ProviderKind::from_str("fake-acp").unwrap(),
            vec![version.clone()],
            vec!["acp".into()],
            ChildSessionPolicy::NotOffered,
        ),
        "never-started-provider",
        std::env::current_dir().unwrap(),
        version,
        Arc::new(NoopAttachmentPathResolver),
    )
    .unwrap()
}

fn harness(
    peer_config: FakeAcpConfig,
    initialize: Option<Value>,
    new_session: Option<Value>,
) -> Harness {
    let (peer, state) = FakeAcpPeer::new(peer_config);
    let injected = Arc::new(Mutex::new(VecDeque::new()));
    let waits = Arc::new(Mutex::new(Vec::new()));
    let script = Script {
        peer,
        injected: injected.clone(),
        waits: waits.clone(),
        initialize,
        new_session,
    };
    Harness {
        control: AcpControl::with_transport(config(), Some(Box::new(script))),
        state,
        injected,
        waits,
    }
}

fn start(control: &mut AcpControl) -> AgentCapabilities {
    control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap()
}
fn input(turn_id: AgentTurnId) -> AgentInput {
    AgentInput {
        turn_id,
        text: "<prompt>".into(),
        attachments: Vec::new(),
        mode: None,
        fast: None,
    }
}
fn drain(control: &mut AcpControl) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for _ in 0..1000 {
        match control.next_event(Duration::ZERO) {
            Ok(Some(event)) => events.push(event),
            Ok(None) | Err(_) => break,
        }
    }
    events
}
fn response(id: u64, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn initialized(http: Value, protocol: Value) -> Value {
    response(
        1,
        json!({"protocolVersion":protocol,"agentCapabilities":{
        "mcpCapabilities":{"http":http,"sse":true},"promptCapabilities":{"image":true}
    },"authMethods":[]}),
    )
}
fn option(id: &str, kind: acp::PermissionOptionKind) -> acp::PermissionOption {
    acp::PermissionOption::new(id.to_owned(), "<label>", kind)
}
fn permission_options() -> Vec<acp::PermissionOption> {
    vec![
        option("<allow>", acp::PermissionOptionKind::AllowOnce),
        option("<reject>", acp::PermissionOptionKind::RejectOnce),
    ]
}
fn approval(events: &[AgentEvent]) -> ApprovalId {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ApprovalRequired { approval_id, .. } => Some(*approval_id),
            _ => None,
        })
        .unwrap()
}
fn terminal(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::TurnFailed { .. }
    )
}

#[test]
fn submit_returns_while_live_and_keeps_caller_id_idempotent_even_after_finish() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    let caps = start(&mut h.control);
    assert_eq!(caps.protocol_version.as_str(), "1");
    assert_eq!(caps.delegation.resume, ResumeSupport::Unsupported);
    assert!(h.control.resume_token().is_none());
    let turn_id = AgentTurnId::mint();
    let before = h.state.lock().unwrap().receives;
    h.control.submit(input(turn_id)).unwrap();
    assert_eq!(
        h.state.lock().unwrap().receives,
        before,
        "submit never waits for response"
    );
    h.control.submit(input(turn_id)).unwrap();
    assert_eq!(h.state.lock().unwrap().prompts, 1);
    let mut conflict = input(turn_id);
    conflict.text.push('x');
    assert_eq!(
        h.control.submit(conflict).unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    let events = drain(&mut h.control);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnStarted { turn_id:id } if *id==turn_id))
            .count(),
        1
    );
    assert!(!events.iter().any(terminal));
    h.injected
        .lock()
        .unwrap()
        .push_back(response(3, json!({"stopReason":"end_turn"})));
    assert!(
        matches!(drain(&mut h.control).as_slice(), [AgentEvent::TurnCompleted { turn_id:id }] if *id==turn_id)
    );
    h.control.submit(input(turn_id)).unwrap();
    assert_eq!(h.state.lock().unwrap().prompts, 1);
    assert!(drain(&mut h.control).is_empty());
    h.control.submit(input(AgentTurnId::mint())).unwrap();
    assert_eq!(h.state.lock().unwrap().prompts, 2);
}

#[test]
fn every_stop_reason_and_unknown_have_distinct_truthful_outcomes() {
    for (stop, completed, cancelled) in [
        ("end_turn", true, false),
        ("cancelled", false, true),
        ("refusal", false, false),
        ("max_tokens", false, false),
        ("max_turn_requests", false, false),
        ("future_stop", false, false),
    ] {
        let mut h = harness(FakeAcpConfig::default(), None, None);
        start(&mut h.control);
        let id = AgentTurnId::mint();
        h.control.submit(input(id)).unwrap();
        h.injected
            .lock()
            .unwrap()
            .push_back(response(3, json!({"stopReason":stop})));
        let events = drain(&mut h.control);
        let terminals: Vec<_> = events.iter().filter(|e| terminal(e)).collect();
        assert_eq!(terminals.len(), 1);
        match terminals[0] {
            AgentEvent::TurnCompleted { turn_id } => {
                assert!(completed);
                assert_eq!(*turn_id, id);
            }
            AgentEvent::TurnCancelled { turn_id, .. } => {
                assert!(cancelled);
                assert_eq!(*turn_id, id);
            }
            AgentEvent::TurnFailed { turn_id, error } => {
                assert!(!completed && !cancelled);
                assert_eq!(*turn_id, id);
                assert_eq!(error.code, ErrorCode::ProviderFailed);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn missing_false_and_malformed_http_remain_managed_but_not_delegatable() {
    for http in [
        Value::Null,
        json!(false),
        json!("true"),
        json!(1),
        json!({}),
    ] {
        let mut h = harness(
            FakeAcpConfig::default(),
            Some(initialized(http, json!(1))),
            None,
        );
        let caps = start(&mut h.control);
        assert_eq!(caps.delegation.mcp, McpSupport::Unsupported);
        assert!(!DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation).delegatable());
        assert!(h.control.mcp_sse);
    }
}

#[test]
fn protocol_integer_is_independent_and_unknown_version_fails() {
    for version in [json!(2), json!("1"), Value::Null] {
        let mut h = harness(
            FakeAcpConfig::default(),
            Some(initialized(json!(true), version)),
            None,
        );
        assert_eq!(
            h.control
                .start(AgentStart::fresh(SessionId::mint(), None))
                .unwrap_err()
                .code,
            ErrorCode::ProviderIncompatible
        );
        assert!(
            !h.state
                .lock()
                .unwrap()
                .sent()
                .iter()
                .any(|frame| frame["method"] == "session/new")
        );
    }
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.executable_version = ProviderVersion::from_str("2.0.0").unwrap();
    assert_eq!(
        h.control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .unwrap_err()
            .diagnostic,
        ProviderDiagnostic::VersionRejected
    );
    assert!(h.state.lock().unwrap().sent().is_empty());
}

fn delegation() -> koru_session_protocol::DelegationLaunch {
    use koru_session_protocol::{DelegationLaunch, McpBearer, McpEndpoint, McpToolAllowlist};
    DelegationLaunch {
        model: None,
        mcp_endpoint: McpEndpoint::from_str("http://127.0.0.1:12345/mcp").unwrap(),
        credential: Some(McpBearer::from_str("secret-bearer-test").unwrap()),
        tools: McpToolAllowlist::default(),
        resume: None,
    }
}

#[test]
fn bearer_only_crosses_session_new_http_headers_and_debug_keeps_it_private() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.delegation = Some(delegation());
    h.control.config.route.session_new_meta = Some(json!({"policy":"<reviewed-policy>"}));
    let caps = start(&mut h.control);
    let state = h.state.lock().unwrap();
    let init = &state.sent()[0];
    assert!(!init.to_string().contains("secret-bearer-test"));
    let params = &state.sent()[1]["params"];
    assert_eq!(params["mcpServers"][0]["type"], "http");
    assert_eq!(
        params["mcpServers"][0]["headers"][0],
        json!({"name":"Authorization","value":"Bearer secret-bearer-test"})
    );
    assert_eq!(params["_meta"]["policy"], "<reviewed-policy>");
    assert!(!format!("{caps:?} {:?}", h.control.diagnostics()).contains("secret-bearer-test"));
    drop(state);
    let mut no_http = harness(
        FakeAcpConfig::default(),
        Some(initialized(json!(false), json!(1))),
        None,
    );
    no_http.control.config.delegation = Some(delegation());
    assert_eq!(
        no_http
            .control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityNotNegotiated
    );
    assert!(
        !no_http
            .state
            .lock()
            .unwrap()
            .sent()
            .iter()
            .any(|frame| frame["method"] == "session/new")
    );
}

#[test]
fn auth_is_checked_at_session_new_and_provider_failure_payload_is_discarded() {
    let error = json!({"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"secret-auth-text","data":{"token":"secret"}}});
    let mut h = harness(FakeAcpConfig::default(), None, Some(error));
    let err = h
        .control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap_err();
    assert_eq!(err.diagnostic, ProviderDiagnostic::AuthenticationRequired);
    assert!(!format!("{err:?} {err}").contains("secret"));
}

#[test]
fn cancel_unanswered_bounds_max_wait_then_refuses_queued_work_and_cleans_once() {
    let mut h = harness(
        FakeAcpConfig {
            ignore_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    drain(&mut h.control);
    h.control.cancel(id).unwrap();
    assert_eq!(h.state.lock().unwrap().cancels, 1);
    h.control.next_event(Duration::MAX).unwrap();
    assert!(*h.waits.lock().unwrap().last().unwrap() <= h.control.config.cancel_timeout);
    h.control
        .turns
        .get_mut(&id)
        .unwrap()
        .cancelling
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    let events = drain(&mut h.control);
    assert!(
        matches!(events.as_slice(),[AgentEvent::TurnCancelled { turn_id, .. }] if *turn_id==id)
    );
    let writes = h.state.lock().unwrap().sent().len();
    assert!(h.control.submit(input(AgentTurnId::mint())).is_err());
    assert!(
        h.control
            .answer_approval(ApprovalId::mint(), ApprovalDecision::Approve, None)
            .is_err()
    );
    assert_eq!(h.state.lock().unwrap().sent().len(), writes);
    assert!(h.control.next_event(Duration::MAX).is_err());
    h.control.shutdown().unwrap();
    h.control.shutdown().unwrap();
    assert_eq!(h.state.lock().unwrap().shutdowns, 1);
}

#[test]
fn cancellation_flood_cannot_starve_wall_clock_deadline() {
    let mut h = harness(
        FakeAcpConfig {
            ignore_cancel: true,
            flood_after_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    h.control.config.cancel_timeout = Duration::from_millis(10);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    drain(&mut h.control);
    h.control.cancel(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(
            Instant::now() < deadline,
            "notification flood starved cancellation"
        );
        if matches!(
            h.control.next_event(Duration::MAX).unwrap(),
            Some(AgentEvent::TurnCancelled { .. })
        ) {
            break;
        }
    }
    assert!(h.control.diagnostics().unknown_events > 0);
    assert!(h.control.next_event(Duration::MAX).is_err());
    h.control.shutdown().unwrap();
    assert_eq!(h.state.lock().unwrap().shutdowns, 1);
}

#[test]
fn immediate_cancel_and_full_content_queue_preserve_started_before_terminal() {
    let mut h = harness(
        FakeAcpConfig {
            ignore_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    h.control.cancel(id).unwrap();
    while h.control.events.len() < h.control.config.event_capacity {
        h.control
            .queue(AgentEvent::MessageDelta {
                turn_id: id,
                text: "<late>".into(),
            })
            .unwrap();
    }
    h.control
        .turns
        .get_mut(&id)
        .unwrap()
        .cancelling
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    assert_eq!(
        h.control.next_event(Duration::MAX).unwrap(),
        Some(AgentEvent::TurnStarted { turn_id: id })
    );
    assert!(matches!(
        h.control.next_event(Duration::MAX).unwrap(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    assert!(h.control.next_event(Duration::MAX).is_err());
    h.control.shutdown().unwrap();
    assert_eq!(h.state.lock().unwrap().shutdowns, 1);
}

#[test]
fn cancel_answers_pending_permission_and_keeps_expiration_before_terminal() {
    let mut h = harness(
        FakeAcpConfig {
            permission_options: Some(permission_options()),
            ignore_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    let pending = approval(&drain(&mut h.control));
    h.control.cancel(id).unwrap();
    assert!(!h.state.lock().unwrap().permission_pending);
    let state = h.state.lock().unwrap();
    let answer = state
        .sent()
        .iter()
        .find(|frame| frame["id"] == PERMISSION_ID)
        .unwrap();
    assert_eq!(answer["result"], json!({"outcome":{"outcome":"cancelled"}}));
    drop(state);
    h.control
        .turns
        .get_mut(&id)
        .unwrap()
        .cancelling
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    assert_eq!(
        h.control.next_event(Duration::MAX).unwrap(),
        Some(AgentEvent::ApprovalExpired {
            turn_id: id,
            approval_id: pending
        })
    );
    assert!(matches!(
        h.control.next_event(Duration::MAX).unwrap(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    assert!(
        h.control
            .answer_approval(pending, ApprovalDecision::Approve, None)
            .is_err()
    );
}

#[test]
fn legacy_unique_permission_selects_once_and_duplicates_never_guess() {
    for duplicate in [false, true] {
        let mut options = permission_options();
        if duplicate {
            options.push(option(
                "<different-allow>",
                acp::PermissionOptionKind::AllowOnce,
            ));
        }
        let mut h = harness(
            FakeAcpConfig {
                permission_options: Some(options),
                ..Default::default()
            },
            None,
            None,
        );
        start(&mut h.control);
        let id = AgentTurnId::mint();
        h.control.submit(input(id)).unwrap();
        let approval_id = approval(&drain(&mut h.control));
        h.control
            .answer_approval(approval_id, ApprovalDecision::Approve, None)
            .unwrap();
        let state = h.state.lock().unwrap();
        let answer = state
            .sent()
            .iter()
            .find(|frame| frame["id"] == PERMISSION_ID)
            .unwrap();
        assert_eq!(
            answer["result"]["outcome"]["outcome"],
            if duplicate { "cancelled" } else { "selected" }
        );
        if !duplicate {
            assert_eq!(answer["result"]["outcome"]["optionId"], "<allow>");
        }
        drop(state);
        let events = drain(&mut h.control);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::ApprovalResolved { .. }))
                .count(),
            usize::from(!duplicate)
        );
        if duplicate {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ApprovalExpired { .. }))
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::TurnFailed { .. }))
            );
        }
    }
}

#[test]
fn completion_wins_late_cancel_and_late_updates_never_make_a_second_terminal() {
    let mut h = harness(
        FakeAcpConfig {
            stop_reason: Some(acp::StopReason::EndTurn),
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    assert_eq!(
        drain(&mut h.control).iter().filter(|e| terminal(e)).count(),
        1
    );
    h.control.cancel(id).unwrap();
    h.injected.lock().unwrap().extend([
        late_update(),
        response(3, json!({"stopReason":"cancelled"})),
    ]);
    assert!(drain(&mut h.control).is_empty());
    assert_eq!(h.state.lock().unwrap().cancels, 0);
}

#[test]
fn unknown_request_gets_method_not_found_and_notifications_are_counted() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    h.injected.lock().unwrap().extend([
        json!({"jsonrpc":"2.0","id":"<private-request>","method":"future/request","params":{"secret":"not forwarded"}}),
        json!({"jsonrpc":"2.0","method":"future/notification","params":{}}),
    ]);
    drain(&mut h.control);
    let state = h.state.lock().unwrap();
    let reply = state.sent().last().unwrap();
    assert_eq!(reply["id"], "<private-request>");
    assert_eq!(reply["error"]["code"], -32601);
    assert!(!reply.to_string().contains("not forwarded"));
    assert_eq!(h.control.diagnostics().unknown_events, 2);
}

#[test]
fn unexpected_eof_emits_session_exited_once_then_error_and_cleanup() {
    let mut h = harness(
        FakeAcpConfig {
            exit_after_prompt: true,
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    let events = drain(&mut h.control);
    assert!(events.iter().any(
        |e| matches!(e,AgentEvent::TurnFailed { error,.. } if error.code==ErrorCode::SessionExited)
    ));
    assert_eq!(events.iter().filter(|e| terminal(e)).count(), 1);
    assert_eq!(
        h.control.next_event(Duration::ZERO).unwrap_err().code,
        ErrorCode::SessionExited
    );
    h.control.shutdown().unwrap();
    assert_eq!(h.state.lock().unwrap().shutdowns, 1);
}

/// Protocol v17 (UC17): a call's input and output cross, bounded, as the
/// agent reported them; its native id never does, and a payload key that is
/// not a message is not a failure's `detail`.
#[test]
fn tool_native_ids_never_cross_and_a_calls_input_and_output_do() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    for update in [
        json!({"sessionUpdate":"tool_call","toolCallId":"<private-tool>","title":"secret-title","kind":"execute","status":"in_progress","rawInput":{"secret":"secret-input"}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"<private-tool>","status":"failed","rawOutput":{"secret":"secret-error"}}),
    ] {
        h.injected.lock().unwrap().push_back(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":SESSION,"update":update}}));
    }
    let events = drain(&mut h.control);
    let debug = format!("{events:?}");
    assert!(!debug.contains("private-tool"));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolStarted { label, input: Some(input), .. }
            if label == "secret-title" && input == r#"{"secret":"secret-input"}"#
    )));
    // The payload's other keys are not a message, so no `detail`; the
    // output is the payload itself, compact.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCompleted {
            succeeded: false,
            failure: Some(koru_session_protocol::ToolFailure::Errored),
            detail: None,
            output: Some(output),
            ..
        } if output == r#"{"secret":"secret-error"}"#
    )));
    let started = events
        .iter()
        .find_map(|e| {
            if let AgentEvent::ToolStarted { tool_call_id, .. } = e {
                Some(*tool_call_id)
            } else {
                None
            }
        })
        .unwrap();
    assert!(events.iter().any(
        |e| matches!(e,AgentEvent::ToolCompleted { tool_call_id,.. } if *tool_call_id==started)
    ));
}

struct ImageBytes;
impl input::ImageReader for ImageBytes {
    fn read(&self, _path: &std::path::Path, _maximum: usize) -> Result<Vec<u8>, ProviderFailure> {
        Ok(vec![0, 1, 2, 3])
    }
}
struct ImagePath;
impl crate::AttachmentPathResolver for ImagePath {
    fn resolve(
        &self,
        _session: SessionId,
        _attachment: koru_session_protocol::AttachmentId,
    ) -> Result<std::path::PathBuf, ErrorCode> {
        Ok(std::env::current_dir().unwrap().join("never-read.png"))
    }
}

#[test]
fn native_images_use_base64_not_path_and_combined_frame_is_bounded() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.attachment_paths = Arc::new(ImagePath);
    h.control.image_reader = Box::new(ImageBytes);
    start(&mut h.control);
    let mut input = input(AgentTurnId::mint());
    input.attachments.push(AgentAttachment {
        id: koru_session_protocol::AttachmentId::mint(),
        kind: koru_session_protocol::AttachmentKind::Image,
        media_type: "image/png".into(),
    });
    h.control.submit(input.clone()).unwrap();
    let state = h.state.lock().unwrap();
    let prompt = state
        .sent()
        .iter()
        .find(|frame| frame["method"] == "session/prompt")
        .unwrap();
    assert_eq!(
        prompt["params"]["prompt"][1],
        json!({"type":"image","mimeType":"image/png","data":"AAECAw=="})
    );
    assert!(!prompt.to_string().contains("never-read.png"));
    drop(state);
    h.control.config.maximum_prompt_frame_bytes = 10;
    assert_eq!(
        h.control.prompt(&input).unwrap_err().code,
        ErrorCode::InputTooLarge
    );
}

struct MebibyteImages;
impl input::ImageReader for MebibyteImages {
    fn read(&self, _path: &std::path::Path, maximum: usize) -> Result<Vec<u8>, ProviderFailure> {
        let bytes = vec![7; 1024 * 1024];
        assert!(bytes.len() <= maximum);
        Ok(bytes)
    }
}

#[test]
fn ten_one_mebibyte_pictures_fit_the_prompt_frame_while_reads_stay_bounded() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.attachment_paths = Arc::new(ImagePath);
    h.control.image_reader = Box::new(MebibyteImages);
    // What the agent sends Koru keeps its small bound; only Koru's own
    // prompt frame grew (chat-attachments D6).
    assert_eq!(
        h.control.config.maximum_inbound_frame_bytes,
        crate::providers::DEFAULT_PROVIDER_FRAME_BYTES
    );
    assert_eq!(
        h.control.config.maximum_prompt_frame_bytes,
        16 * 1024 * 1024
    );
    start(&mut h.control);
    let mut input = input(AgentTurnId::mint());
    for _ in 0..10 {
        input.attachments.push(AgentAttachment {
            id: koru_session_protocol::AttachmentId::mint(),
            kind: koru_session_protocol::AttachmentKind::Image,
            media_type: "image/png".into(),
        });
    }
    let prompt = h.control.prompt(&input).unwrap();
    assert_eq!(prompt.len(), 11, "the text and ten pictures");
    // Thirteen are past the frame and refused before anything is sent.
    for _ in 0..3 {
        input.attachments.push(AgentAttachment {
            id: koru_session_protocol::AttachmentId::mint(),
            kind: koru_session_protocol::AttachmentKind::Image,
            media_type: "image/png".into(),
        });
    }
    assert_eq!(
        h.control.prompt(&input).unwrap_err().code,
        ErrorCode::InputTooLarge
    );
}

#[test]
fn silent_cancel_also_reaches_real_deadline_without_forcing_clock() {
    let mut h = harness(
        FakeAcpConfig {
            ignore_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    h.control.config.cancel_timeout = Duration::from_millis(5);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    drain(&mut h.control);
    h.control.cancel(id).unwrap();
    let bound = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < bound);
        if matches!(
            h.control.next_event(Duration::MAX).unwrap(),
            Some(AgentEvent::TurnCancelled { .. })
        ) {
            break;
        }
    }
    assert!(h.control.next_event(Duration::MAX).is_err());
}

#[test]
fn malformed_or_error_response_fails_without_raw_payload_and_refuses_queued_work() {
    for frame in [
        json!({"jsonrpc":"wrong", "method":"secret-wire", "params":{"token":"secret"}}),
        json!({"jsonrpc":"2.0", "id":3, "error":{"code":-32603, "message":"secret", "data":{"path":"secret"}}}),
    ] {
        let mut h = harness(FakeAcpConfig::default(), None, None);
        start(&mut h.control);
        let id = AgentTurnId::mint();
        h.control.submit(input(id)).unwrap();
        h.injected.lock().unwrap().push_back(frame);
        let events = drain(&mut h.control);
        assert_eq!(events.iter().filter(|e| terminal(e)).count(), 1);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnFailed { .. }))
        );
        assert!(!format!("{events:?}").contains("secret"));
    }
}

#[test]
fn cancel_write_failure_is_terminal_and_never_leaves_an_accepting_adapter() {
    let mut h = harness(
        FakeAcpConfig {
            fail_at: Some(ProviderStage::Cancel),
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    assert!(h.control.cancel(id).is_err());
    assert!(h.control.submit(input(AgentTurnId::mint())).is_err());
    let events = drain(&mut h.control);
    assert!(matches!(events.first(), Some(AgentEvent::TurnStarted { turn_id }) if *turn_id == id));
    assert_eq!(events.iter().filter(|e| terminal(e)).count(), 1);
    assert!(h.control.next_event(Duration::MAX).is_err());
}

#[test]
fn approval_write_failure_expires_only_pending_approval_and_forces_cleanup() {
    let mut h = harness(
        FakeAcpConfig {
            fail_at: Some(ProviderStage::Approval),
            permission_options: Some(permission_options()),
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    h.control.submit(input(AgentTurnId::mint())).unwrap();
    let id = approval(&drain(&mut h.control));
    assert!(
        h.control
            .answer_approval(id, ApprovalDecision::Approve, None)
            .is_err()
    );
    let writes = h.state.lock().unwrap().sent().len();
    assert!(
        h.control
            .answer_approval(id, ApprovalDecision::Approve, None)
            .is_err()
    );
    assert_eq!(h.state.lock().unwrap().sent().len(), writes);
    let events = drain(&mut h.control);
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ApprovalExpired { approval_id, .. } if *approval_id == id)
    ));
    assert_eq!(events.iter().filter(|e| terminal(e)).count(), 1);
    assert!(h.control.next_event(Duration::MAX).is_err());
    h.control.shutdown().unwrap();
    assert_eq!(h.state.lock().unwrap().shutdowns, 1);
}

#[test]
fn permission_reservation_survives_a_full_content_queue() {
    let mut h = harness(
        FakeAcpConfig {
            permission_options: Some(permission_options()),
            ignore_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    let id = approval(&drain(&mut h.control));
    while h
        .control
        .queue(AgentEvent::MessageDelta {
            turn_id: turn,
            text: "<content>".into(),
        })
        .is_ok()
    {}
    h.control.cancel(turn).unwrap();
    assert!(!h.state.lock().unwrap().permission_pending);
    assert_eq!(h.state.lock().unwrap().cancels, 1);
    h.control
        .turns
        .get_mut(&turn)
        .unwrap()
        .cancelling
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    let events = drain(&mut h.control);
    assert!(
        matches!(events.as_slice(), [AgentEvent::ApprovalExpired { approval_id, .. }, AgentEvent::TurnCancelled { .. }] if *approval_id == id)
    );
    h.control.shutdown().unwrap();
    assert_eq!(h.state.lock().unwrap().shutdowns, 1);
}

#[test]
fn late_permission_while_cancelling_is_declined_without_opening_an_approval() {
    let mut h = harness(
        FakeAcpConfig {
            ignore_cancel: true,
            ..Default::default()
        },
        None,
        None,
    );
    start(&mut h.control);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    drain(&mut h.control);
    h.control.cancel(turn).unwrap();
    h.injected.lock().unwrap().push_back(json!({"jsonrpc":"2.0","id":"<late-permission>",
        "method":"session/request_permission", "params":{"sessionId":SESSION,
        "toolCall":{"toolCallId":"<private-tool>","title":"<summary>"}, "options":permission_options()}}));
    assert!(drain(&mut h.control).is_empty());
    assert!(h.control.approvals.is_empty());
    let state = h.state.lock().unwrap();
    let response = state
        .sent()
        .iter()
        .find(|frame| frame["id"] == "<late-permission>")
        .unwrap();
    assert_eq!(
        response["result"],
        json!({"outcome":{"outcome":"cancelled"}})
    );
}

#[test]
fn correlated_handshakes_require_exact_jsonrpc_and_exclusive_result_or_error() {
    for id in [1, 2] {
        let good = if id == 1 {
            initialized(json!(true), json!(1))
        } else {
            response(2, json!({"sessionId":SESSION}))
        };
        for variant in 0..5 {
            let mut bad = good.clone();
            match variant {
                0 => {
                    bad.as_object_mut().unwrap().remove("jsonrpc");
                }
                1 => {
                    bad["jsonrpc"] = json!("1.0");
                }
                2 => {
                    bad["error"] = json!({"code":-32000,"message":"secret-auth"});
                }
                3 => {
                    bad.as_object_mut().unwrap().remove("result");
                }
                4 => {
                    bad.as_object_mut().unwrap().remove("result");
                    bad["error"] = json!({"message":"secret-auth"});
                }
                _ => unreachable!(),
            }
            let mut h = if id == 1 {
                harness(FakeAcpConfig::default(), Some(bad), None)
            } else {
                harness(FakeAcpConfig::default(), None, Some(bad))
            };
            let error = h
                .control
                .start(AgentStart::fresh(SessionId::mint(), None))
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ProviderIncompatible);
            assert_eq!(error.diagnostic, ProviderDiagnostic::HandshakeRejected);
            assert!(!format!("{error:?}").contains("secret"));
        }
    }
}

/// A killed runtime's folder goes at the next launch; this process's own,
/// and anything not named like one, stay.
#[test]
fn a_dead_runtimes_temp_folder_is_swept_and_a_live_ones_kept() {
    let temp = tempfile::tempdir().unwrap();
    let mut finished = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--list")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let dead = finished.id();
    finished.wait().unwrap();
    let own = std::process::id();
    let names = [
        format!("koru-acp-{dead}-0"),
        format!("koru-acp-{own}-0"),
        "koru-acp-notes".to_owned(),
    ];
    for name in &names {
        std::fs::create_dir(temp.path().join(name)).unwrap();
        std::fs::write(temp.path().join(name).join("payload"), b"x").unwrap();
    }

    sweep_abandoned_temp(temp.path());

    assert!(!temp.path().join(&names[0]).exists(), "the dead runtime's");
    assert!(temp.path().join(&names[1]).exists(), "this runtime's own");
    assert!(temp.path().join(&names[2]).exists(), "not a runtime's");
    assert_eq!(temp_owner("koru-acp-12-3"), Some(12));
    assert_eq!(temp_owner("koru-acp-12"), None);
}

mod antigravity;

mod approvals;

mod claude;

mod routes;

mod routes_live;

mod diagnostics;

/// Protocol v17: a failed call's own message crosses, bounded — the first
/// text block of the update, else the raw output's `message` — and a
/// completed call carries none.
#[test]
fn a_failed_tool_carries_its_own_message_bounded() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    let long = "x".repeat(1_000);
    for update in [
        json!({"sessionUpdate":"tool_call","toolCallId":"call-1","title":"use_tool","kind":"other","status":"in_progress"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"failed",
            "content":[{"type":"content","content":{"type":"text","text":"  Tool `koru__list_workspaces` failed: Tool not found  "}}],
            "rawOutput":{"error":"tool_execution_failed","message":"Tool not found"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"call-2","title":"grep","kind":"search","status":"in_progress"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call-2","status":"failed","rawOutput":{"message":long}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"call-3","title":"read","kind":"read","status":"in_progress"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call-3","status":"completed","content":[{"type":"content","content":{"type":"text","text":"a result"}}]}),
    ] {
        h.injected.lock().unwrap().push_back(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":SESSION,"update":update}}));
    }
    let events = drain(&mut h.control);
    let details: Vec<(bool, Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCompleted {
                succeeded,
                detail,
                output,
                ..
            } => Some((*succeeded, detail.clone(), output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(details.len(), 3);
    assert_eq!(
        details[0],
        (
            false,
            Some("Tool `koru__list_workspaces` failed: Tool not found".into()),
            Some("Tool `koru__list_workspaces` failed: Tool not found".into())
        )
    );
    assert_eq!(
        details[1].1.as_deref().map(str::len),
        Some(koru_session_protocol::TOOL_DETAIL_CHARS)
    );
    assert_eq!(details[2], (true, None, Some("a result".into())));
}

/// UC17: a diff block crosses as its path over its new text, an update's
/// content accumulates until the completion, and everything is bounded.
#[test]
fn a_tools_output_is_its_text_or_its_diff_and_is_bounded() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    let long = "y".repeat(10_000);
    for update in [
        json!({"sessionUpdate":"tool_call","toolCallId":"edit-1","title":"Edit README.md","kind":"edit","status":"in_progress","rawInput":{"path":"README.md"}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"edit-1","content":[{"type":"diff","path":"README.md","oldText":"old","newText":"new words"}]}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"edit-1","status":"completed"}),
        json!({"sessionUpdate":"tool_call","toolCallId":"run-1","title":"Bash","kind":"execute","status":"in_progress","rawInput":"ls -la"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"run-1","status":"completed","content":[{"type":"content","content":{"type":"text","text":long}}]}),
    ] {
        h.injected.lock().unwrap().push_back(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":SESSION,"update":update}}));
    }
    let events = drain(&mut h.control);
    let inputs: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { input, .. } => Some(input.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        inputs,
        vec![
            Some(r#"{"path":"README.md"}"#.into()),
            Some("ls -la".into())
        ]
    );
    let outputs: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCompleted { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(outputs[0].as_deref(), Some("README.md\nnew words"));
    assert_eq!(
        outputs[1].as_deref().map(str::len),
        Some(koru_session_protocol::TOOL_CONTENT_CHARS)
    );
}

fn inject_updates(h: &mut Harness, updates: impl IntoIterator<Item = Value>) {
    for update in updates {
        h.injected.lock().unwrap().push_back(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":SESSION,"update":update}}));
    }
}

fn starts(events: &[AgentEvent]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { label, input, .. } => Some((label.clone(), input.clone())),
            _ => None,
        })
        .collect()
}

fn outputs(events: &[AgentEvent]) -> Vec<Option<String>> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCompleted { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect()
}

/// D17: Claude (claude-agent-acp 0.76.0, `toolCallNotification`) announces
/// a call as *Terminal* with `rawInput: {}` while the model is still writing
/// it, then refines it in an update with the command, its real title and
/// the command's description as text. The row starts once, with the
/// command; the description is not the output; the result is.
#[test]
fn a_call_announced_before_its_input_starts_with_what_it_does() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    inject_updates(
        &mut h,
        [
            json!({"sessionUpdate":"tool_call","toolCallId":"bash-1","title":"Terminal","kind":"execute","status":"pending","rawInput":{},"content":[]}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"bash-1","title":"git status","kind":"execute",
                "rawInput":{"command":"git status","description":"Show the tree's status"},
                "content":[{"type":"content","content":{"type":"text","text":"Show the tree's status"}}]}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"bash-1","status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":"```console\nOn branch main\n```"}}],
                "rawOutput":"On branch main"}),
        ],
    );
    let events = drain(&mut h.control);
    assert_eq!(
        starts(&events),
        vec![(
            "git status".to_owned(),
            Some(r#"{"command":"git status","description":"Show the tree's status"}"#.into())
        )]
    );
    assert_eq!(
        outputs(&events),
        vec![Some("```console\nOn branch main\n```".into())]
    );
}

/// Calls announced together start in the order they were announced, each
/// once it says what it does; text that follows a held call releases it
/// first, so the call keeps its place in the reply.
#[test]
fn held_calls_start_in_order_and_before_what_follows_them() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    inject_updates(
        &mut h,
        [
            json!({"sessionUpdate":"tool_call","toolCallId":"a","title":"Read File","kind":"read","status":"pending","rawInput":{}}),
            json!({"sessionUpdate":"tool_call","toolCallId":"b","title":"Read File","kind":"read","status":"pending","rawInput":{}}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"b","title":"Read b.rs","rawInput":{"file_path":"b.rs"}}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"a","title":"Read a.rs","rawInput":{"file_path":"a.rs"}}),
            json!({"sessionUpdate":"tool_call","toolCallId":"c","title":"Terminal","kind":"execute","status":"pending","rawInput":{}}),
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"and then"}}),
        ],
    );
    let events = drain(&mut h.control);
    let labels = starts(&events)
        .into_iter()
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
    assert_eq!(labels, ["Read a.rs", "Read b.rs", "Terminal"]);
    let held = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolStarted { label, .. } if label == "Terminal"))
        .unwrap();
    let said = events
        .iter()
        .position(|e| matches!(e, AgentEvent::MessageDelta { .. }))
        .unwrap();
    assert!(held < said, "a held call starts before the text after it");
}

/// An edit's diff is its output even when the completion's `rawOutput`
/// says only that the file was updated.
#[test]
fn an_edits_diff_outlives_the_completions_raw_output() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let id = AgentTurnId::mint();
    h.control.submit(input(id)).unwrap();
    inject_updates(
        &mut h,
        [
            json!({"sessionUpdate":"tool_call","toolCallId":"e","title":"Edit","kind":"edit","status":"pending","rawInput":{}}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"e","title":"Edit a.txt",
                "rawInput":{"file_path":"a.txt","old_string":"old","new_string":"new"},
                "content":[{"type":"diff","path":"a.txt","oldText":"old","newText":"new"}]}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed",
                "rawOutput":"The file a.txt has been updated."}),
        ],
    );
    let events = drain(&mut h.control);
    assert_eq!(starts(&events)[0].0, "Edit a.txt");
    let output = outputs(&events).remove(0).unwrap();
    assert!(output.contains("a.txt"), "{output}");
    assert!(!output.contains("has been updated"), "{output}");
}

/// A session that offers models and, on the one it starts on, levels.
fn leveled_peer() -> FakeAcpConfig {
    let effort = |current: &str, values: &[&str]| {
        json!({"id":"effort","type":"select","category":"thought_level","currentValue":current,
            "options":values.iter().map(|value| json!({"value":value,"name":value})).collect::<Vec<_>>()})
    };
    FakeAcpConfig {
        config_options: vec![
            json!({"id":"model","type":"select","category":"model","currentValue":"default",
                "options":[{"value":"default","name":"Default"},{"value":"opus","name":"Opus"},
                           {"value":"haiku","name":"Haiku"}]}),
            effort("default", &["default", "low", "high"]),
        ],
        thought_by_model: vec![
            (
                "default".into(),
                effort("default", &["default", "low", "high"]),
            ),
            (
                "opus".into(),
                effort("default", &["default", "low", "high", "max"]),
            ),
        ],
        ..FakeAcpConfig::default()
    }
}

fn select(
    model: Option<&str>,
    reasoning: Option<koru_session_protocol::LaunchReasoning>,
) -> Option<LaunchSelection> {
    Some(LaunchSelection {
        model: model.map(|model| model.parse().unwrap()),
        reasoning,
        mode: None,
        fast: None,
    })
}

/// **D18: the level is set after the model, from the model's own answer,
/// and checked applied.**
#[test]
fn a_chosen_level_is_set_after_the_model_and_checked() {
    let mut h = harness(leveled_peer(), None, None);
    h.control.config.selection = select(
        Some("opus"),
        Some(koru_session_protocol::LaunchReasoning::Max),
    );
    start(&mut h.control);
    let state = h.state.lock().unwrap();
    let sets: Vec<(String, String)> = state
        .sent()
        .iter()
        .filter(|message| message["method"] == "session/set_config_option")
        .map(|message| {
            (
                message["params"]["configId"].as_str().unwrap().to_owned(),
                message["params"]["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        sets,
        [
            ("model".into(), "opus".into()),
            ("effort".into(), "max".into())
        ],
        "max is opus's, offered only once opus is set"
    );
}

/// **D19: a level the session does not offer, or does not report applied,
/// fails the start as a refused level** — never a run at another level.
#[test]
fn a_level_not_offered_or_not_applied_is_refused() {
    let mut h = harness(leveled_peer(), None, None);
    h.control.config.selection = select(None, Some(koru_session_protocol::LaunchReasoning::Max));
    let refused = h
        .control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::ProviderModelUnavailable);
    assert_eq!(refused.diagnostic, ProviderDiagnostic::ReasoningRejected);

    let mut ignoring = leveled_peer();
    ignoring.ignore_level = true;
    let mut h = harness(ignoring, None, None);
    h.control.config.selection = select(None, Some(koru_session_protocol::LaunchReasoning::High));
    let refused = h
        .control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap_err();
    assert_eq!(refused.diagnostic, ProviderDiagnostic::ReasoningRejected);

    // A session with no level option at all refuses any level.
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.selection = select(None, Some(koru_session_protocol::LaunchReasoning::Low));
    assert_eq!(
        h.control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .unwrap_err()
            .diagnostic,
        ProviderDiagnostic::ReasoningRejected
    );
}

/// **D16: the walk reads each model's own levels** by setting the session's
/// model to it; a model with none reads none.
#[test]
fn the_walk_reads_each_models_levels() {
    let mut h = harness(leveled_peer(), None, None);
    start(&mut h.control);
    let walked = h.control.walk_levels();
    let levels = |id: &str| {
        walked
            .iter()
            .find(|model| model.id.as_str() == id)
            .unwrap()
            .reasoning
            .as_ref()
            .map(|reasoning| reasoning.levels.len())
    };
    assert_eq!(levels("default"), Some(2));
    assert_eq!(levels("opus"), Some(3));
    assert_eq!(levels("haiku"), None);
}
