// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_session_protocol::{
    AgentEvent, AgentTurnId, AgentUsage, ApprovalDecision, AttachmentId, AttachmentKind, ErrorCode,
    SessionId, ToolFailure, ToolKind, capability,
};
use serde_json::Value;

use super::*;
use crate::providers::AgentAttachment;
use crate::{AttachmentPathResolver, NoopAttachmentPathResolver};

const SECRET: &str = "PROVIDER_SECRET_SENTINEL_92741";

#[derive(Default)]
struct ReplayState {
    sent: Vec<Value>,
    shutdowns: usize,
}

struct ReplayTransport {
    inbound: VecDeque<Value>,
    state: Arc<Mutex<ReplayState>>,
}

impl JsonRpcTransport for ReplayTransport {
    fn send(&mut self, message: &Value) -> Result<(), ProviderFailure> {
        self.state.lock().unwrap().sent.push(message.clone());
        Ok(())
    }

    fn receive(&mut self, _timeout: Duration) -> Result<TransportReceive, ProviderFailure> {
        Ok(self
            .inbound
            .pop_front()
            .map_or(TransportReceive::Timeout, TransportReceive::Frame))
    }

    fn shutdown(&mut self, _timeout: Duration) -> Result<(), ProviderFailure> {
        self.state.lock().unwrap().shutdowns += 1;
        Ok(())
    }
}

struct OnePath {
    id: AttachmentId,
    path: PathBuf,
}

impl AttachmentPathResolver for OnePath {
    fn resolve(
        &self,
        _session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<PathBuf, ErrorCode> {
        if attachment_id == self.id {
            Ok(self.path.clone())
        } else {
            Err(ErrorCode::AttachmentNotFound)
        }
    }
}

/// The sanitized server transcripts the offline replay drives the adapter
/// with, one per supported CLI version. Same session, same normalized
/// events; what differs is the shape each version speaks.
const REPLAY_0_150_1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.150.1/adapter-replay.server.jsonl"
));
const REPLAY_0_153_4: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.153.4/adapter-replay.server.jsonl"
));
const REPLAY_0_154_0: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.154.0/adapter-replay.server.jsonl"
));

fn replay_for(
    version: &str,
    transcript: &str,
) -> (CodexControl, Arc<Mutex<ReplayState>>, AttachmentId) {
    let fixture_path = std::env::temp_dir().join("koru-codex-fixture-probe.png");
    let fixture_path_text = fixture_path.to_string_lossy().into_owned();
    let mut inbound: VecDeque<Value> = transcript
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for frame in &mut inbound {
        replace_fixture_value(frame, "<image-path>", &fixture_path_text);
        replace_fixture_value(
            frame,
            "<codex-home>",
            &std::env::current_dir().unwrap().to_string_lossy(),
        );
    }
    let state = Arc::new(Mutex::new(ReplayState::default()));
    let attachment_id = AttachmentId::mint();
    let resolver = Arc::new(OnePath {
        id: attachment_id,
        path: fixture_path,
    });
    let config = CodexConfig::new(
        "codex",
        std::env::current_dir().unwrap(),
        ProviderVersion::from_str(version).unwrap(),
        resolver,
    )
    .unwrap();
    let transport = ReplayTransport {
        inbound,
        state: Arc::clone(&state),
    };
    (
        CodexControl::with_transport(config, Some(Box::new(transport))),
        state,
        attachment_id,
    )
}

fn replace_fixture_value(value: &mut Value, marker: &str, replacement: &str) {
    match value {
        Value::String(value) if value == marker => *value = replacement.to_owned(),
        Value::Array(values) => {
            for value in values {
                replace_fixture_value(value, marker, replacement);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                replace_fixture_value(value, marker, replacement);
            }
        }
        _ => {}
    }
}

fn next(control: &mut CodexControl) -> AgentEvent {
    control
        .next_event(Duration::ZERO)
        .unwrap()
        .expect("fixture event")
}

fn config(resolver: Arc<dyn AttachmentPathResolver>) -> CodexConfig {
    CodexConfig::new(
        "codex",
        std::env::current_dir().unwrap(),
        ProviderVersion::from_str("0.150.1").unwrap(),
        resolver,
    )
    .unwrap()
}

fn replay_control(
    inbound: impl IntoIterator<Item = Value>,
    resolver: Arc<dyn AttachmentPathResolver>,
) -> (CodexControl, Arc<Mutex<ReplayState>>) {
    let state = Arc::new(Mutex::new(ReplayState::default()));
    let transport = ReplayTransport {
        inbound: inbound.into_iter().collect(),
        state: Arc::clone(&state),
    };
    (
        CodexControl::with_transport(config(resolver), Some(Box::new(transport))),
        state,
    )
}

fn initialize_frames(image: bool) -> Vec<Value> {
    vec![
        json!({
            "id": 1,
            "result": {
                "userAgent": "koru_terminal/0.150.1 (fixture)",
                "codexHome": "fixture-home",
                "platformFamily": "fixture-family",
                "platformOs": "fixture-os"
            }
        }),
        json!({
            "id": 2,
            "result": {
                "data": [{
                    "isDefault": true,
                    "model": "fixture-default",
                    "inputModalities": if image { json!(["text", "image"]) } else { json!(["text"]) }
                }],
                "nextCursor": null
            }
        }),
        json!({ "id": 3, "result": { "thread": { "id": "fixture-thread" } } }),
    ]
}

#[test]
fn paginated_model_list_selects_one_default_and_forwards_each_cursor() {
    let (mut control, state) = replay_control(
        [
            json!({
                "id": 1,
                "result": {
                    "data": [{
                        "isDefault": false,
                        "model": "not-selected",
                        "inputModalities": ["text"]
                    }],
                    "nextCursor": "second-page"
                }
            }),
            json!({
                "id": 2,
                "result": {
                    "data": [{
                        "isDefault": true,
                        "model": "selected-default",
                        "inputModalities": ["text", "image"]
                    }],
                    "nextCursor": null
                }
            }),
        ],
        Arc::new(NoopAttachmentPathResolver),
    );
    let selected = control.select_model(None).unwrap();
    assert_eq!(selected.model, "selected-default");
    assert!(selected.image);
    assert_eq!(
        state.lock().unwrap().sent,
        vec![
            json!({ "method": "model/list", "id": 1, "params": { "limit": 100 } }),
            json!({
                "method": "model/list",
                "id": 2,
                "params": { "cursor": "second-page", "limit": 100 }
            }),
        ]
    );

    let (mut duplicate, _) = replay_control(
        [
            json!({
                "id": 1,
                "result": {
                    "data": [{
                        "isDefault": true,
                        "model": "first-default",
                        "inputModalities": ["text"]
                    }],
                    "nextCursor": "again"
                }
            }),
            json!({
                "id": 2,
                "result": {
                    "data": [{
                        "isDefault": true,
                        "model": "second-default",
                        "inputModalities": ["text"]
                    }],
                    "nextCursor": null
                }
            }),
        ],
        Arc::new(NoopAttachmentPathResolver),
    );
    let failure = duplicate.select_model(None).unwrap_err();
    assert_eq!(failure.code, ErrorCode::ProviderIncompatible);
    assert_eq!(failure.stage, ProviderStage::Initialize);
    assert_eq!(failure.diagnostic, ProviderDiagnostic::HandshakeRejected);
}

#[test]
fn initialize_identity_and_required_response_fields_are_exact() {
    let (mut control, state) = replay_control(
        initialize_frames(true),
        Arc::new(NoopAttachmentPathResolver),
    );
    control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    let sent = &state.lock().unwrap().sent;
    assert_eq!(
        sent[0],
        json!({
            "method": "initialize",
            "id": 1,
            "params": {
                "clientInfo": {
                    "name": "koru_terminal",
                    "title": "Koru Terminal",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": false }
            }
        })
    );
    assert_eq!(sent[1], json!({ "method": "initialized", "params": {} }));
    assert_eq!(
        sent[3].pointer("/params/model"),
        Some(&json!("fixture-default"))
    );

    let version = ProviderVersion::from_str("0.150.1").unwrap();
    let valid = json!({
        "userAgent": "koru_terminal/0.150.1 (fixture)",
        "codexHome": "fixture-home",
        "platformFamily": "fixture-family",
        "platformOs": "fixture-os"
    });
    validate_initialize(&valid, &version).unwrap();
    for required in ["userAgent", "codexHome", "platformFamily", "platformOs"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(required);
        let failure = validate_initialize(&missing, &version).unwrap_err();
        assert_eq!(failure.code, ErrorCode::ProviderIncompatible, "{required}");
        assert_eq!(failure.diagnostic, ProviderDiagnostic::HandshakeRejected);
    }
    let wrong_identity = json!({
        "userAgent": "another_client/0.150.1",
        "codexHome": "fixture-home",
        "platformFamily": "fixture-family",
        "platformOs": "fixture-os"
    });
    assert_eq!(
        validate_initialize(&wrong_identity, &version)
            .unwrap_err()
            .diagnostic,
        ProviderDiagnostic::HandshakeRejected
    );
}

#[test]
fn text_only_default_does_not_advertise_or_accept_images() {
    let (mut control, state) = replay_control(
        initialize_frames(false),
        Arc::new(NoopAttachmentPathResolver),
    );
    let capabilities = control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    assert!(
        !capabilities
            .capabilities
            .supports(capability::NATIVE_ATTACHMENT_DELIVERY)
    );
    let failure = control
        .submit(AgentInput {
            turn_id: AgentTurnId::mint(),
            text: "inspect".into(),
            attachments: vec![AgentAttachment {
                id: AttachmentId::mint(),
                kind: AttachmentKind::Image,
                media_type: "image/png".into(),
            }],
        })
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::UnsupportedMedia);
    assert_eq!(failure.stage, ProviderStage::Submit);
    assert!(
        !state
            .lock()
            .unwrap()
            .sent
            .iter()
            .any(|message| { message.get("method").and_then(Value::as_str) == Some("turn/start") })
    );
}

#[test]
fn an_unknown_turn_start_outcome_is_never_resent_and_kind_is_idempotent_data() {
    let attachment_id = AttachmentId::mint();
    let resolver = Arc::new(OnePath {
        id: attachment_id,
        path: std::env::temp_dir().join("koru-idempotency-probe.png"),
    });
    let (mut control, state) = replay_control(initialize_frames(true), resolver);
    control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    let input = AgentInput {
        turn_id: AgentTurnId::mint(),
        text: "one logical turn".into(),
        attachments: vec![AgentAttachment {
            id: attachment_id,
            kind: AttachmentKind::Image,
            media_type: "image/png".into(),
        }],
    };
    let first = control.submit(input.clone()).unwrap_err();
    assert_eq!(first.code, ErrorCode::ProviderUnavailable);
    assert_eq!(first.stage, ProviderStage::Submit);
    assert_eq!(first.diagnostic, ProviderDiagnostic::Timeout);
    let repeated = control.submit(input.clone()).unwrap_err();
    assert_eq!(repeated, first);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|message| {
                message.get("method").and_then(Value::as_str) == Some("turn/start")
            })
            .count(),
        1
    );

    let changed_kind = AgentInput {
        attachments: vec![AgentAttachment {
            id: attachment_id,
            kind: AttachmentKind::File,
            media_type: "image/png".into(),
        }],
        ..input
    };
    let conflict = control.submit(changed_kind).unwrap_err();
    assert_eq!(conflict.code, ErrorCode::IdempotencyConflict);
    assert_eq!(conflict.diagnostic, ProviderDiagnostic::InvalidInput);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|message| {
                message.get("method").and_then(Value::as_str) == Some("turn/start")
            })
            .count(),
        1
    );
}

#[test]
fn approval_resolution_and_expiration_are_one_shot_and_content_free() {
    let turn_id = AgentTurnId::mint();
    let native_turn = "native-approval-turn";
    let (mut control, state) = replay_control(
        [
            json!({
                "method": "item/commandExecution/requestApproval",
                "id": 40,
                "params": {
                    "threadId": "fixture-thread",
                    "turnId": native_turn,
                    "command": SECRET,
                    "reason": SECRET
                }
            }),
            json!({
                "method": "item/fileChange/requestApproval",
                "id": "provider-request-41",
                "params": {
                    "threadId": "fixture-thread",
                    "turnId": native_turn,
                    "reason": SECRET
                }
            }),
            json!({
                "method": "serverRequest/resolved",
                "params": {
                    "threadId": "fixture-thread",
                    "requestId": "provider-request-41",
                    "resolution": SECRET
                }
            }),
            json!({
                "method": "item/permissions/requestApproval",
                "id": 42,
                "params": {
                    "threadId": "fixture-thread",
                    "turnId": native_turn,
                    "reason": SECRET,
                    "permissions": { "network": { "enabled": true } }
                }
            }),
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "fixture-thread",
                    "turn": {
                        "id": native_turn,
                        "status": "failed",
                        "error": SECRET
                    }
                }
            }),
            json!({
                "method": "future/request",
                "id": 43,
                "params": { "payload": SECRET }
            }),
        ],
        Arc::new(NoopAttachmentPathResolver),
    );
    control.thread_id = Some("fixture-thread".into());
    control.turns.insert(
        turn_id,
        NativeTurn {
            native_id: Some(native_turn.into()),
            fingerprint: blake3::hash(b"fixture"),
            terminal: false,
        },
    );
    control.native_turns.insert(native_turn.into(), turn_id);

    let resolved = match next(&mut control) {
        AgentEvent::ApprovalRequired {
            turn_id: observed,
            approval_id,
            summary,
            ..
        } => {
            assert_eq!(observed, turn_id);
            assert_eq!(summary, "command_execution");
            approval_id
        }
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    let resolved_choice = control.approvals[&resolved]
        .choices
        .iter()
        .find(|choice| choice.intent == koru_session_protocol::ApprovalIntent::Reject)
        .unwrap()
        .id;
    control
        .answer_approval(resolved, ApprovalDecision::Reject, None)
        .unwrap();
    assert_eq!(
        next(&mut control),
        AgentEvent::ApprovalResolved {
            turn_id,
            approval_id: resolved,
            decision: ApprovalDecision::Reject,
            choice: Some(resolved_choice),
        }
    );
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        control
            .answer_approval(resolved, ApprovalDecision::Approve, None)
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );

    let provider_resolved = match next(&mut control) {
        AgentEvent::ApprovalRequired { approval_id, .. } => approval_id,
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        next(&mut control),
        AgentEvent::ApprovalExpired {
            turn_id,
            approval_id: provider_resolved,
        }
    );
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        control
            .answer_approval(provider_resolved, ApprovalDecision::Reject, None)
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );

    let turn_expired = match next(&mut control) {
        AgentEvent::ApprovalRequired { approval_id, .. } => approval_id,
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        next(&mut control),
        AgentEvent::ApprovalExpired {
            turn_id,
            approval_id: turn_expired,
        }
    );
    match next(&mut control) {
        AgentEvent::TurnFailed {
            turn_id: observed,
            error,
        } => {
            assert_eq!(observed, turn_id);
            assert_eq!(error.code, ErrorCode::ProviderFailed);
            assert_eq!(error.message, ErrorCode::ProviderFailed.as_str());
            assert!(!error.message.contains(SECRET));
        }
        event => panic!("unexpected event: {event:?}"),
    }
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));

    assert_eq!(control.next_event(Duration::ZERO).unwrap(), None);
    let diagnostics = control.diagnostics();
    assert_eq!(diagnostics.unknown_events, 1);
    assert!(!format!("{diagnostics:?}").contains(SECRET));
    let sent = &state.lock().unwrap().sent;
    let error_payloads = sent
        .iter()
        .filter(|message| message.get("error").is_some())
        .collect::<Vec<_>>();
    assert_eq!(error_payloads.len(), 1);
    assert!(
        !serde_json::to_string(&error_payloads)
            .unwrap()
            .contains(SECRET)
    );
    let rejected = validate_initialize(
        &json!({
            "userAgent": SECRET,
            "codexHome": SECRET,
            "platformFamily": SECRET,
            "platformOs": SECRET
        }),
        &ProviderVersion::from_str("0.150.1").unwrap(),
    )
    .unwrap_err();
    assert!(!format!("{rejected:?} {rejected}").contains(SECRET));
}

/// The same session against each supported version's transcript. 0.150.1
/// names no model anywhere, so the reported model is an honest `None`;
/// 0.153.4 names the thread's model on `thread/start` and reroutes this
/// turn, and the usage report says which model served it.
#[test]
fn offline_replay_maps_text_tools_usage_approval_image_cancel_and_cleanup() {
    offline_replay("0.150.1", REPLAY_0_150_1, None, None);
}

#[test]
fn offline_replay_against_0_153_4_reports_the_thread_model_and_a_reroute() {
    offline_replay(
        "0.153.4",
        REPLAY_0_153_4,
        Some("default-model"),
        Some("fixture-rerouted"),
    );
}

#[test]
fn offline_replay_against_0_154_0_preserves_the_delegation_contract() {
    offline_replay(
        "0.154.0",
        REPLAY_0_154_0,
        Some("default-model"),
        Some("fixture-rerouted"),
    );
}

fn offline_replay(
    version: &str,
    transcript: &str,
    thread_model: Option<&str>,
    usage_model: Option<&str>,
) {
    let (mut control, state, attachment_id) = replay_for(version, transcript);
    let session_id = SessionId::mint();
    let capabilities = control
        .start(AgentStart {
            session_id,
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    assert!(
        capabilities
            .capabilities
            .supports(capability::MANAGED_EVENTS)
    );
    assert!(
        capabilities
            .capabilities
            .supports(capability::MANAGED_CANCEL)
    );
    assert!(
        capabilities
            .capabilities
            .supports(capability::MANAGED_APPROVAL)
    );
    assert!(
        capabilities
            .capabilities
            .supports(capability::MANAGED_USAGE)
    );
    assert!(
        capabilities
            .capabilities
            .supports(capability::NATIVE_ATTACHMENT_DELIVERY)
    );
    assert_eq!(
        capabilities
            .delegation
            .economics
            .model
            .reported
            .as_ref()
            .map(|model| model.as_str()),
        thread_model,
        "{version}: the model the thread reported at start"
    );
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        control
            .start(AgentStart {
                session_id: SessionId::mint(),
                parent_session_id: None,
                resume: None,
            })
            .unwrap_err()
            .code,
        ErrorCode::InvalidTransition
    );

    let turn_id = AgentTurnId::mint();
    let input = AgentInput {
        turn_id,
        text: SECRET.to_owned(),
        attachments: vec![AgentAttachment {
            id: attachment_id,
            kind: AttachmentKind::Image,
            media_type: "image/png".into(),
        }],
    };
    control.submit(input.clone()).unwrap();
    control.submit(input.clone()).unwrap();
    let conflicting = AgentInput {
        text: "different".into(),
        ..input
    };
    assert_eq!(
        control.submit(conflicting).unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(next(&mut control), AgentEvent::TurnStarted { turn_id });
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        next(&mut control),
        AgentEvent::MessageDelta {
            turn_id,
            text: "PRO".into(),
        }
    );

    // A consumer can detach here: the provider remains owned by the session,
    // bounded transport buffering continues, and reattachment resumes events.
    assert_eq!(
        next(&mut control),
        AgentEvent::MessageDelta {
            turn_id,
            text: "BE_OK".into(),
        }
    );
    let tool_call_id = match next(&mut control) {
        AgentEvent::ToolStarted {
            turn_id: observed,
            tool_call_id,
            label,
            ..
        } => {
            assert_eq!(observed, turn_id);
            assert_eq!(label, "command_execution");
            tool_call_id
        }
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(
        next(&mut control),
        AgentEvent::ToolCompleted {
            turn_id,
            tool_call_id,
            succeeded: true,
            tool: Some(ToolKind::CommandExecution),
            failure: None,
            detail: None,
            output: None,
        }
    );
    match next(&mut control) {
        AgentEvent::Usage {
            turn_id: observed,
            usage,
        } => {
            assert_eq!(observed, turn_id);
            assert_eq!(
                usage.model.as_ref().map(|model| model.as_str()),
                usage_model,
                "{version}: the model the provider said served the turn"
            );
        }
        event => panic!("unexpected event: {event:?}"),
    }
    let approval_id = match next(&mut control) {
        AgentEvent::ApprovalRequired {
            turn_id: observed,
            approval_id,
            summary,
            ..
        } => {
            assert_eq!(observed, turn_id);
            assert_eq!(summary, "command_execution");
            approval_id
        }
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    control
        .answer_approval(approval_id, ApprovalDecision::Reject, None)
        .unwrap();
    assert!(matches!(
        next(&mut control),
        AgentEvent::ApprovalResolved { .. }
    ));
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        control
            .answer_approval(approval_id, ApprovalDecision::Approve, None)
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );
    let file_approval = match next(&mut control) {
        AgentEvent::ApprovalRequired {
            approval_id,
            summary,
            ..
        } => {
            assert_eq!(summary, "file_change");
            approval_id
        }
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    control
        .answer_approval(file_approval, ApprovalDecision::Approve, None)
        .unwrap();
    assert!(matches!(
        next(&mut control),
        AgentEvent::ApprovalResolved { .. }
    ));
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    let permission_approval = match next(&mut control) {
        AgentEvent::ApprovalRequired {
            approval_id,
            summary,
            ..
        } => {
            assert_eq!(summary, "permissions");
            approval_id
        }
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    control
        .answer_approval(permission_approval, ApprovalDecision::Approve, None)
        .unwrap();
    assert!(matches!(
        next(&mut control),
        AgentEvent::ApprovalResolved { .. }
    ));
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    let provider_resolved = match next(&mut control) {
        AgentEvent::ApprovalRequired { approval_id, .. } => approval_id,
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        next(&mut control),
        AgentEvent::ApprovalExpired {
            turn_id,
            approval_id: provider_resolved,
        }
    );
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    let expires_with_turn = match next(&mut control) {
        AgentEvent::ApprovalRequired { approval_id, .. } => approval_id,
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        next(&mut control),
        AgentEvent::AttachmentAccepted {
            turn_id,
            attachment_id,
        }
    );
    assert_eq!(
        next(&mut control),
        AgentEvent::ApprovalExpired {
            turn_id,
            approval_id: expires_with_turn,
        }
    );
    assert_eq!(next(&mut control), AgentEvent::TurnCompleted { turn_id });
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    assert_eq!(
        control
            .answer_approval(expires_with_turn, ApprovalDecision::Approve, None)
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );

    let cancelled = AgentTurnId::mint();
    control
        .submit(AgentInput {
            turn_id: cancelled,
            text: "cancel fixture".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    assert_eq!(
        next(&mut control),
        AgentEvent::TurnStarted { turn_id: cancelled }
    );
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    control.cancel(cancelled).unwrap();
    assert_eq!(
        next(&mut control),
        AgentEvent::TurnCancelled {
            turn_id: cancelled,
            reason: Default::default(),
        }
    );
    assert!(matches!(
        next(&mut control),
        AgentEvent::ProviderState { .. }
    ));
    control.cancel(cancelled).unwrap();

    assert_eq!(control.diagnostics().unknown_events, 3);
    control.shutdown().unwrap();
    control.shutdown().unwrap();
    assert_eq!(state.lock().unwrap().shutdowns, 1);

    let sent = &state.lock().unwrap().sent;
    assert_eq!(
        sent.iter()
            .filter(|message| message.get("method").and_then(Value::as_str) == Some("turn/start"))
            .count(),
        2
    );
    let approval = sent
        .iter()
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(90))
        .unwrap();
    assert_eq!(
        approval.pointer("/result/decision"),
        Some(&Value::String("decline".into()))
    );
    let file_approval = sent
        .iter()
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(91))
        .unwrap();
    assert_eq!(
        file_approval.pointer("/result/decision"),
        Some(&Value::String("accept".into()))
    );
    let permission_approval = sent
        .iter()
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(92))
        .unwrap();
    assert_eq!(
        permission_approval.pointer("/result/permissions/network/enabled"),
        Some(&Value::Bool(true))
    );
    let unsupported_request = sent
        .iter()
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(93))
        .unwrap();
    assert_eq!(
        unsupported_request.pointer("/error/code"),
        Some(&Value::from(-32601))
    );
    let unknown_turn = sent
        .iter()
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(94))
        .unwrap();
    assert_eq!(
        unknown_turn.pointer("/error/code"),
        Some(&Value::from(-32602))
    );
}

#[test]
fn incompatible_versions_and_unadvertised_images_fail_closed() {
    let config = CodexConfig::new(
        "codex",
        std::env::current_dir().unwrap(),
        ProviderVersion::from_str("0.151.0").unwrap(),
        Arc::new(NoopAttachmentPathResolver),
    )
    .unwrap();
    let mut control = CodexControl::with_transport(
        config,
        Some(Box::new(ReplayTransport {
            inbound: VecDeque::new(),
            state: Arc::new(Mutex::new(ReplayState::default())),
        })),
    );
    assert_eq!(
        control
            .start(AgentStart {
                session_id: SessionId::mint(),
                parent_session_id: None,
                resume: None,
            })
            .unwrap_err()
            .code,
        ErrorCode::ProviderIncompatible
    );
}

#[test]
fn checked_in_schema_evidence_matches_the_adapter_boundary() {
    for (version, evidence, digest) in [
        (
            "0.150.1",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.150.1/schema-evidence.json"
            )),
            "8cdccfc35582696d7141e7f916e0d5a664ab5b5e90b732f104284d2507f369f8",
        ),
        (
            "0.153.4",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.153.4/schema-evidence.json"
            )),
            "d3eace08be5dca386bfd1f1e8df650058b4113f1e10870a284d775d75517576a",
        ),
        (
            "0.154.0",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.154.0/schema-evidence.json"
            )),
            "f3487938786b729cb6773dbc9e83a7efab9c78c845db7094e8f539f373cbacc9",
        ),
        (
            "0.156.1",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.156.1/schema-evidence.json"
            )),
            "995fc3b8f8c469f6787e8fc5be4038c4f31359025edd8480b862e83355f3bf3b",
        ),
        (
            "0.158.0",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.158.0/schema-evidence.json"
            )),
            "5742a9a7dd41a8b44dca3138f506e013620d4a93573c792b1e5881c053f169a7",
        ),
        (
            "0.163.0",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.163.0/schema-evidence.json"
            )),
            "d204b96d8560d56600127423b89df9db471b3e22338e8809bb91d5a952998978",
        ),
        (
            "0.165.0",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/codex-app-server/0.165.0/schema-evidence.json"
            )),
            "38027ec8a6fffc2422b674cab13dc1a05c26e13a48349c46a274609d52090e35",
        ),
    ] {
        let evidence: Value = serde_json::from_str(evidence).unwrap();
        assert!(SUPPORTED_CODEX_VERSIONS.contains(&version), "{version}");
        assert_eq!(evidence["cli_version"], version);
        assert_eq!(evidence["protocol_version"], CODEX_PROTOCOL_VERSION);
        assert_eq!(evidence["schema_sha256"], digest, "{version}");
    }
    assert_eq!(
        SUPPORTED_CODEX_VERSIONS.len(),
        7,
        "every supported version has evidence"
    );

    let text_only = json!({
        "data": [{
            "isDefault": true,
            "model": "fixture",
            "inputModalities": ["text"]
        }]
    });
    let page = normalize::model_page(&text_only).unwrap();
    assert_eq!(page.models.len(), 1);
    assert!(page.models[0].is_default);
    assert!(!page.models[0].image);
}

#[test]
#[ignore = "requires an authenticated compatible Codex CLI; run explicitly"]
fn live_codex_text_and_image_smoke_is_opt_in() {
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31,
        0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("probe.png");
    std::fs::write(&image, PNG).unwrap();
    let attachment_id = AttachmentId::mint();
    let resolver = Arc::new(OnePath {
        id: attachment_id,
        path: image,
    });
    let executable = installed_codex();
    let installed = CodexCompatibilityProbe::new(
        executable.clone(),
        std::env::current_dir().unwrap(),
        Arc::new(NoopAttachmentPathResolver),
    )
    .version(crate::providers::ProbeLimits::default())
    .expect("codex --version answers");
    let config = CodexConfig::new(
        executable,
        std::env::current_dir().unwrap(),
        installed,
        resolver,
    )
    .unwrap();
    let mut control = CodexControl::new(config);
    control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    let turn_id = AgentTurnId::mint();
    control
        .submit(AgentInput {
            turn_id,
            text: "Reply with the single word OK after inspecting this one-pixel image.".into(),
            attachments: vec![AgentAttachment {
                id: attachment_id,
                kind: AttachmentKind::Image,
                media_type: "image/png".into(),
            }],
        })
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut saw_text = false;
    let mut saw_image = false;
    while std::time::Instant::now() < deadline {
        match control.next_event(Duration::from_secs(1)).unwrap() {
            Some(AgentEvent::MessageDelta { .. }) => saw_text = true,
            Some(AgentEvent::AttachmentAccepted { .. }) => saw_image = true,
            Some(AgentEvent::ApprovalRequired { approval_id, .. }) => {
                control
                    .answer_approval(approval_id, ApprovalDecision::Reject, None)
                    .unwrap();
            }
            Some(AgentEvent::TurnCompleted { .. }) => break,
            Some(AgentEvent::TurnFailed { error, .. }) => {
                panic!("live turn failed: {:?}", error.code)
            }
            _ => {}
        }
    }
    assert!(saw_text);
    assert!(saw_image);
    control.cancel(turn_id).unwrap();
    control.shutdown().unwrap();
}

/// What the adapter declares about delegation, and the verdict computed
/// from it. Every fact here is what `docs/features/terminal/providers/codex.md`
/// records; changing one without the other is what this test catches.
#[test]
fn codex_declares_its_delegation_facts_and_is_delegatable() {
    use crate::providers::{
        ChildSessionPolicy, DelegationVerdict, McpSupport, ResumeSupport, UsageReporting,
    };
    use koru_session_protocol::CachedInputRelationship;

    let (mut control, _) = replay_control(
        initialize_frames(false),
        Arc::new(NoopAttachmentPathResolver),
    );
    let capabilities = control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap();
    let facts = &capabilities.delegation;
    assert!(facts.starts_in_cwd);
    assert!(facts.distinguishes_outcomes);
    assert!(facts.follows_up);
    assert!(facts.reports_tool_activity);
    assert_eq!(facts.resume, ResumeSupport::Unsupported);
    assert_eq!(facts.mcp, McpSupport::Supported);
    assert_eq!(facts.child_sessions, ChildSessionPolicy::Disabled);
    assert_eq!(facts.economics.usage, UsageReporting::RunningTotal);
    assert_eq!(
        facts.economics.cached_input,
        CachedInputRelationship::Included
    );
    assert_eq!(
        facts
            .economics
            .model
            .requested
            .as_ref()
            .map(|model| model.as_str()),
        Some("fixture-default")
    );
    assert_eq!(facts.economics.model.reported, None);
    let verdict = DelegationVerdict::evaluate(&capabilities.capabilities, facts);
    assert!(verdict.delegatable(), "{:?}", verdict.missing());
    assert_eq!(control.resume_token(), None);
}

/// No native resume is declared, so no token was ever minted, and a start
/// that carries one is a caller ignoring the declaration.
#[test]
fn a_resume_token_is_refused_because_none_was_ever_minted() {
    let (mut control, state) = replay_control(
        initialize_frames(false),
        Arc::new(NoopAttachmentPathResolver),
    );
    let failure = control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: Some("thread-from-another-life".parse().unwrap()),
        })
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
    assert_eq!(failure.stage, ProviderStage::Initialize);
    assert!(state.lock().unwrap().sent.is_empty(), "nothing was sent");
}

fn usage_frame(turn: &str, input: u64, output: u64, cached: Option<u64>) -> Value {
    let mut total = json!({
        "inputTokens": input,
        "outputTokens": output,
        "reasoningOutputTokens": 0,
        "totalTokens": input + output
    });
    if let Some(cached) = cached {
        total["cachedInputTokens"] = json!(cached);
    }
    json!({
        "method": "thread/tokenUsage/updated",
        "params": {
            "threadId": "fixture-thread",
            "turnId": turn,
            "tokenUsage": { "last": total.clone(), "total": total, "modelContextWindow": 1000 }
        }
    })
}

fn turn_completed(turn: &str, status: &str) -> Value {
    json!({
        "method": "turn/completed",
        "params": {
            "threadId": "fixture-thread",
            "turn": { "id": turn, "status": status, "items": [] }
        }
    })
}

fn turn_started(id: u64, turn: &str) -> Value {
    json!({ "id": id, "result": { "turn": { "id": turn, "status": "inProgress", "items": [] } } })
}

fn drain_usage(control: &mut CodexControl) -> Vec<(AgentTurnId, AgentUsage)> {
    let mut usage = Vec::new();
    while let Some(event) = control.next_event(Duration::ZERO).unwrap() {
        if let AgentEvent::Usage {
            turn_id,
            usage: report,
        } = event
        {
            usage.push((turn_id, report));
        }
    }
    usage
}

fn totals(usage: &AgentUsage) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
    )
}

/// **Section 17.2's normalization, against the shapes the app-server
/// sends.** The thread total is differenced from the turn's baseline, so a
/// repeated total is the same turn total, a lower total is a correction, a
/// report after completion or cancellation still names its turn, a missing
/// field is unavailable rather than zero, cached input is declared
/// included, and a second turn in the same thread starts from where the
/// first left off. A fresh attempt is a fresh thread, and starts at zero.
#[test]
fn codex_usage_is_normalized_to_one_replacing_total_per_turn() {
    use koru_session_protocol::CachedInputRelationship;

    let mut frames = initialize_frames(false);
    frames.push(turn_started(4, "t1"));
    frames.push(usage_frame("t1", 11, 7, Some(3)));
    frames.push(usage_frame("t1", 11, 7, Some(3)));
    frames.push(usage_frame("t1", 9, 6, Some(2)));
    frames.push(usage_frame("t1", 12, 8, None));
    frames.push(turn_completed("t1", "completed"));
    frames.push(usage_frame("t1", 20, 10, Some(5)));
    let (mut control, _) = replay_control(frames, Arc::new(NoopAttachmentPathResolver));
    control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap();
    let first = AgentTurnId::mint();
    control
        .submit(AgentInput {
            turn_id: first,
            text: "first".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    let reports = drain_usage(&mut control);
    assert_eq!(reports.len(), 5);
    assert!(reports.iter().all(|(turn, _)| *turn == first));
    assert_eq!(totals(&reports[0].1), (Some(11), Some(7), Some(3)));
    assert_eq!(
        totals(&reports[1].1),
        (Some(11), Some(7), Some(3)),
        "repeated, not doubled"
    );
    assert_eq!(
        totals(&reports[2].1),
        (Some(9), Some(6), Some(2)),
        "corrected downward"
    );
    assert_eq!(
        totals(&reports[3].1),
        (Some(12), Some(8), None),
        "missing is unavailable"
    );
    assert_eq!(
        totals(&reports[4].1),
        (Some(20), Some(10), Some(5)),
        "late report applied"
    );
    for (_, report) in &reports {
        assert_eq!(report.cached_input, CachedInputRelationship::Included);
        assert_eq!(report.model, None, "Codex 0.150.1 names no model per usage");
    }

    // A second turn in the same thread is measured from the thread total at
    // its submission (20/10/5), through a cancellation and its late report.
    let second = AgentTurnId::mint();
    let frames = vec![
        turn_started(5, "t2"),
        usage_frame("t2", 25, 12, Some(6)),
        json!({ "id": 6, "result": {} }),
        turn_completed("t2", "interrupted"),
        usage_frame("t2", 26, 12, Some(6)),
    ];
    control.transport = Some(Box::new(ReplayTransport {
        inbound: frames.into_iter().collect(),
        state: Arc::new(Mutex::new(ReplayState::default())),
    }));
    control
        .submit(AgentInput {
            turn_id: second,
            text: "second".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    control.cancel(second).unwrap();
    let mut saw_cancelled = false;
    let mut reports = Vec::new();
    while let Some(event) = control.next_event(Duration::ZERO).unwrap() {
        match event {
            AgentEvent::Usage { turn_id, usage } => reports.push((turn_id, usage)),
            AgentEvent::TurnCancelled { turn_id, .. } => saw_cancelled |= turn_id == second,
            _ => {}
        }
    }
    assert!(saw_cancelled);
    assert_eq!(reports.len(), 2);
    assert!(reports.iter().all(|(turn, _)| *turn == second));
    assert_eq!(totals(&reports[0].1), (Some(5), Some(2), Some(1)));
    assert_eq!(
        totals(&reports[1].1),
        (Some(6), Some(2), Some(1)),
        "after cancellation"
    );

    let mut frames = initialize_frames(false);
    frames.push(turn_started(4, "t1"));
    frames.push(usage_frame("t1", 4, 2, Some(1)));
    let (mut fresh, _) = replay_control(frames, Arc::new(NoopAttachmentPathResolver));
    fresh
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap();
    let attempt = AgentTurnId::mint();
    fresh
        .submit(AgentInput {
            turn_id: attempt,
            text: "again".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    let reports = drain_usage(&mut fresh);
    assert_eq!(reports.len(), 1);
    assert_eq!(totals(&reports[0].1), (Some(4), Some(2), Some(1)));
}

/// **The opt-in live smoke: a real Codex reaches a stub MCP endpoint.**
///
/// Never runs under plain `cargo test`: it needs `KORU_LIVE_PROVIDER_SMOKE`
/// set and an authenticated Codex CLI at the pinned version on `PATH`, and
/// it talks to a real provider. What it proves that no fixture can is that
/// the rendered `-c mcp_servers.koru_run.url` override makes the provider
/// process open a connection to the endpoint Koru named, with the bearer
/// override accepted and the variable it names set on the child. The
/// endpoint here is a bare listener, so the MCP handshake fails after the
/// connection, and that is fine: reaching it is the claim. What the run
/// endpoint answers is the delegate worker's own process test.
#[test]
fn live_codex_reaches_a_stub_mcp_endpoint_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    use koru_session_protocol::{DelegationLaunch, McpToolAllowlist};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let reached = Arc::new(Mutex::new(false));
    let seen = Arc::clone(&reached);
    listener.set_nonblocking(true).unwrap();
    let acceptor = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if listener.accept().is_ok() {
                *seen.lock().unwrap() = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let delegation = DelegationLaunch {
        model: None,
        mcp_endpoint: format!("http://127.0.0.1:{port}/mcp").parse().unwrap(),
        // A bearer as well, so the live CLI has to accept
        // `bearer_token_env_var` and find the variable set on its own
        // process — the two halves of ADR-359 that no fixture can prove.
        credential: Some("koru_run_livesmokelivesmokelivesmoke".parse().unwrap()),
        tools: McpToolAllowlist::new(
            [
                "attach_to_run",
                "create",
                "create_version",
                "get",
                "read",
                "search",
            ]
            .iter()
            .map(|name| name.parse().unwrap()),
        )
        .unwrap(),
        resume: None,
    };
    // The installed CLI is the one under test: probe its version rather than
    // assuming a pin, and let the adapter's own gate say whether it is one
    // this adapter supports.
    let executable = installed_codex();
    let installed = CodexCompatibilityProbe::new(
        executable.clone(),
        std::env::current_dir().unwrap(),
        Arc::new(NoopAttachmentPathResolver),
    )
    .version(crate::providers::ProbeLimits::default())
    .expect("codex --version answers");
    let config = CodexConfig::new(
        executable,
        std::env::current_dir().unwrap(),
        installed,
        Arc::new(NoopAttachmentPathResolver),
    )
    .unwrap()
    .with_delegation(Some(delegation));
    let mut control = CodexControl::new(config);
    control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap();
    let turn_id = AgentTurnId::mint();
    control
        .submit(AgentInput {
            turn_id,
            text: "Reply with the single word OK.".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut kinds = Vec::new();
    while std::time::Instant::now() < deadline && !*reached.lock().unwrap() {
        match control.next_event(Duration::from_secs(1)).unwrap() {
            Some(AgentEvent::ApprovalRequired { approval_id, .. }) => {
                kinds.push("approval_required");
                control
                    .answer_approval(approval_id, ApprovalDecision::Reject, None)
                    .unwrap();
            }
            Some(AgentEvent::TurnCompleted { .. }) => {
                kinds.push("turn_completed");
                break;
            }
            Some(AgentEvent::TurnFailed { .. }) => {
                kinds.push("turn_failed");
                break;
            }
            Some(AgentEvent::Usage { usage, .. }) => {
                kinds.push(if usage.model.is_some() {
                    "usage(model reported)"
                } else {
                    "usage"
                });
            }
            Some(AgentEvent::MessageDelta { .. }) => kinds.push("message_delta"),
            Some(AgentEvent::ToolStarted { .. }) => kinds.push("tool_started"),
            Some(AgentEvent::ToolCompleted { .. }) => kinds.push("tool_completed"),
            Some(_) => kinds.push("other"),
            None => {}
        }
    }
    // Record the finding before the teardown, which through a Windows `.cmd`
    // shim can time out and would otherwise hide it.
    let reached = *reached.lock().unwrap();
    eprintln!(
        "live smoke: endpoint reached = {reached}; event kinds = {kinds:?}; \n         unknown notifications = {}",
        control.diagnostics().unknown_events
    );
    let _ = control.cancel(turn_id);
    let shutdown = control.shutdown();
    eprintln!("live smoke: orderly shutdown = {shutdown:?}");
    acceptor.join().unwrap();
    assert!(
        reached,
        "the provider never connected to the stub MCP endpoint"
    );
}

/// The installed CLI as the session host would resolve it. The npm install
/// is a `codex.cmd` shim on Windows, which `Command::new("codex")` cannot see
/// through `PATHEXT` the way the host's profile resolver can; a live smoke
/// that looked for the bare name would report a missing executable on every
/// Windows machine that has Codex.
fn installed_codex() -> std::ffi::OsString {
    let names: &[&str] = if cfg!(windows) {
        &["codex.cmd", "codex.exe", "codex"]
    } else {
        &["codex"]
    };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .find(|candidate| candidate.is_file())
        .map(|found| found.into_os_string())
        .unwrap_or_else(|| "codex".into())
}

// -------------------------------------- which tool failed, and how (P3c)

/// Eight fixture strings, one per locale, plus a path. **None of them may
/// leave the adapter**: a provider error is read for whether it is there
/// and never for what it says, so a run's history can name the failure
/// without ever holding a word of it.
const ERROR_SENTINELS: [&str; 9] = [
    "TOOL-ERROR-SENTINEL command not found",
    "TOOL-ERROR-SENTINEL orden no encontrada",
    "TOOL-ERROR-SENTINEL comando nao encontrado",
    "TOOL-ERROR-SENTINEL Befehl nicht gefunden",
    "TOOL-ERROR-SENTINEL commande introuvable",
    "TOOL-ERROR-SENTINEL الأمر غير موجود",
    "TOOL-ERROR-SENTINEL 未找到命令",
    "TOOL-ERROR-SENTINEL コマンドが見つかりません",
    "C:\\TOOL-PATH-SENTINEL\\private\\notes.md",
];

fn tool_item(native_id: &str, kind: &str, status: &str, error: Option<Value>) -> Value {
    let mut item = json!({ "id": native_id, "type": kind, "status": status });
    if let Some(error) = error {
        item["error"] = error;
    }
    item
}

fn tool_turn(frames: Vec<Value>) -> (CodexControl, AgentTurnId) {
    let turn_id = AgentTurnId::mint();
    let native_turn = "native-tool-turn";
    let (mut control, _) = replay_control(frames, Arc::new(NoopAttachmentPathResolver));
    control.thread_id = Some("fixture-thread".into());
    control.turns.insert(
        turn_id,
        NativeTurn {
            native_id: Some(native_turn.into()),
            fingerprint: blake3::hash(b"fixture"),
            terminal: false,
        },
    );
    control.native_turns.insert(native_turn.into(), turn_id);
    (control, turn_id)
}

fn tool_frames(calls: &[(&str, &str, &str, Option<Value>)]) -> Vec<Value> {
    let mut frames = Vec::new();
    for (native_id, kind, status, error) in calls {
        frames.push(json!({
            "method": "item/started",
            "params": {
                "threadId": "fixture-thread",
                "turnId": "native-tool-turn",
                "item": tool_item(native_id, kind, "inProgress", None),
            }
        }));
        frames.push(json!({
            "method": "item/completed",
            "params": {
                "threadId": "fixture-thread",
                "turnId": "native-tool-turn",
                "item": tool_item(native_id, kind, status, error.clone()),
            }
        }));
    }
    frames
}

/// The classifier, one status at a time. Everything the adapter has
/// evidence for is placed; **everything else is `Unclassified`, never a
/// guess and never a silence**.
#[test]
fn a_failed_item_is_classified_from_its_status_or_left_unclassified() {
    let cases: [(&str, Option<Value>, Option<ToolFailure>); 9] = [
        ("completed", None, None),
        ("completed", Some(Value::Null), None),
        ("failed", None, Some(ToolFailure::Errored)),
        ("cancelled", None, Some(ToolFailure::Cancelled)),
        ("rejected", None, Some(ToolFailure::Denied)),
        ("timedOut", None, Some(ToolFailure::TimedOut)),
        // A status this build has never seen, with nothing beside it.
        ("wedged", None, Some(ToolFailure::Unclassified)),
        // The same, with the provider having named an error: it errored,
        // which is all that is known and all that is said.
        (
            "wedged",
            Some(json!({ "message": ERROR_SENTINELS[0] })),
            Some(ToolFailure::Errored),
        ),
        // `completed` with an error beside it is still a failure.
        (
            "completed",
            Some(json!({ "message": ERROR_SENTINELS[8] })),
            Some(ToolFailure::Errored),
        ),
    ];
    for (status, error, expected) in cases {
        let item = tool_item("item-1", "commandExecution", status, error);
        assert_eq!(
            normalize::item_failure(&item),
            expected,
            "status {status:?} classified wrongly"
        );
    }
}

#[test]
fn every_tool_the_adapter_names_arrives_as_its_token_and_its_class() {
    let calls: Vec<(&str, &str, &str, Option<Value>)> = vec![
        ("a", "commandExecution", "failed", None),
        ("b", "fileChange", "rejected", None),
        ("c", "mcpToolCall", "cancelled", None),
        ("d", "dynamicToolCall", "timedOut", None),
        ("e", "webSearch", "wedged", None),
        ("f", "imageGeneration", "completed", None),
    ];
    let (mut control, turn_id) = tool_turn(tool_frames(&calls));
    let expected = [
        (ToolKind::CommandExecution, Some(ToolFailure::Errored)),
        (ToolKind::FileChange, Some(ToolFailure::Denied)),
        (ToolKind::McpTool, Some(ToolFailure::Cancelled)),
        (ToolKind::DynamicTool, Some(ToolFailure::TimedOut)),
        (ToolKind::WebSearch, Some(ToolFailure::Unclassified)),
        (ToolKind::ImageGeneration, None),
    ];
    for (tool, failure) in expected {
        let started = next(&mut control);
        let AgentEvent::ToolStarted { label, .. } = &started else {
            panic!("expected a start, got {started:?}");
        };
        // The label the session view has always shown is the token itself.
        assert_eq!(label, tool.as_str());
        match next(&mut control) {
            AgentEvent::ToolCompleted {
                turn_id: observed,
                succeeded,
                tool: reported,
                failure: reported_failure,
                ..
            } => {
                assert_eq!(observed, turn_id);
                assert_eq!(reported, Some(tool));
                assert_eq!(reported_failure, failure);
                assert_eq!(succeeded, failure.is_none());
            }
            event => panic!("expected a completion, got {event:?}"),
        }
    }
}

/// **Standing requirement 7 at the adapter's edge, as amended for the
/// chat (protocol v17).** A provider error is classified into a token, and
/// of its text exactly one field crosses: `message`, bounded, as the
/// completion's `detail` — the sentence a person asks for on a failed row.
/// Its other fields, its localized copies, its path and its command reach
/// no event, no `Debug`, no JSON.
#[test]
fn a_provider_error_crosses_as_its_message_and_nothing_else() {
    let error = json!({
        "message": ERROR_SENTINELS[0],
        "detail": ERROR_SENTINELS[1],
        "localized": [
            ERROR_SENTINELS[2],
            ERROR_SENTINELS[3],
            ERROR_SENTINELS[4],
            ERROR_SENTINELS[5],
            ERROR_SENTINELS[6],
            ERROR_SENTINELS[7],
        ],
        "path": ERROR_SENTINELS[8],
        "command": SECRET,
    });
    let calls: Vec<(&str, &str, &str, Option<Value>)> =
        vec![("a", "commandExecution", "failed", Some(error))];
    let (mut control, _) = tool_turn(tool_frames(&calls));
    let mut rendered = Vec::new();
    for _ in 0..2 {
        let event = next(&mut control);
        rendered.push(format!("{event:?}"));
        rendered.push(serde_json::to_string(&event).unwrap());
    }
    for text in &rendered {
        for sentinel in &ERROR_SENTINELS[1..] {
            assert!(!text.contains(sentinel), "{sentinel} leaked into {text}");
        }
        assert!(!text.contains(SECRET), "{SECRET} leaked into {text}");
    }
    // And what it *does* say is the classification, and the message.
    assert!(
        rendered.iter().any(|text| text.contains("errored")),
        "{rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|text| text.contains(ERROR_SENTINELS[0])),
        "the message is the one field that crosses: {rendered:?}"
    );
}

/// An older peer sends neither field. Reading its event is not an error and
/// not a guess: the tool and the class are simply absent.
#[test]
fn a_completion_from_an_older_peer_carries_neither_tool_nor_class() {
    let turn_id = AgentTurnId::mint();
    let tool_call_id = ToolCallId::mint();
    let event: AgentEvent = serde_json::from_value(json!({
        "kind": "tool_completed",
        "turn_id": turn_id,
        "tool_call_id": tool_call_id,
        "succeeded": false,
    }))
    .unwrap();
    assert_eq!(
        event,
        AgentEvent::ToolCompleted {
            turn_id,
            tool_call_id,
            succeeded: false,
            tool: None,
            failure: None,
            detail: None,
            output: None,
        }
    );
}

#[test]
fn a_chosen_model_starts_the_thread_and_an_unlisted_one_is_refused() {
    let frames = vec![
        json!({
            "id": 1,
            "result": {
                "userAgent": "koru_terminal/0.150.1 (fixture)",
                "codexHome": "fixture-home",
                "platformFamily": "fixture-family",
                "platformOs": "fixture-os"
            }
        }),
        json!({
            "id": 2,
            "result": {
                "data": [
                    { "isDefault": true, "model": "fixture-default", "inputModalities": ["text"] },
                    { "isDefault": false, "model": "fixture-other", "inputModalities": ["text", "image"] }
                ],
                "nextCursor": null
            }
        }),
        json!({ "id": 3, "result": { "thread": { "id": "fixture-thread" } } }),
    ];
    let selection = |model: &str| {
        Some(koru_session_protocol::LaunchSelection {
            model: Some(model.parse().unwrap()),
            reasoning: None,
        })
    };
    let state = Arc::new(Mutex::new(ReplayState::default()));
    let transport = ReplayTransport {
        inbound: frames.clone().into_iter().collect(),
        state: Arc::clone(&state),
    };
    let mut control = CodexControl::with_transport(
        config(Arc::new(NoopAttachmentPathResolver)).with_selection(selection("fixture-other")),
        Some(Box::new(transport)),
    );
    let capabilities = control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    let thread_start = state.lock().unwrap().sent[3].clone();
    assert_eq!(thread_start["method"], json!("thread/start"));
    assert_eq!(thread_start["params"]["model"], json!("fixture-other"));
    assert_eq!(
        capabilities.delegation.economics.model.requested,
        Some("fixture-other".parse().unwrap())
    );

    let state = Arc::new(Mutex::new(ReplayState::default()));
    let transport = ReplayTransport {
        inbound: frames.into_iter().collect(),
        state: Arc::clone(&state),
    };
    let mut control = CodexControl::with_transport(
        config(Arc::new(NoopAttachmentPathResolver)).with_selection(selection("not-listed")),
        Some(Box::new(transport)),
    );
    let failure = control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::ProviderModelUnavailable);
    let sent = state.lock().unwrap().sent.clone();
    assert_eq!(
        sent.len(),
        3,
        "initialize, initialized, one model page; no thread"
    );
}

#[test]
fn discovery_lists_every_text_model_under_one_group_and_shuts_down() {
    let (mut control, state) = replay_control(
        [
            json!({
                "id": 1,
                "result": {
                    "userAgent": "koru_terminal/0.150.1 (fixture)",
                    "codexHome": "fixture-home",
                    "platformFamily": "fixture-family",
                    "platformOs": "fixture-os"
                }
            }),
            json!({
                "id": 2,
                "result": {
                    "data": [
                        { "isDefault": true, "model": "fixture-default", "inputModalities": ["text"] },
                        { "isDefault": false, "model": "fixture-other", "inputModalities": ["text"] },
                        { "isDefault": false, "model": "voice-only", "inputModalities": ["audio"] }
                    ],
                    "nextCursor": null
                }
            }),
        ],
        Arc::new(NoopAttachmentPathResolver),
    );
    let models = control.discover_models().unwrap();
    let ids: Vec<&str> = models
        .models()
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(ids, vec!["fixture-default", "fixture-other"]);
    assert!(
        models
            .models()
            .iter()
            .all(|model| model.group.as_str() == "openai")
    );
    let sent = state.lock().unwrap().sent.clone();
    assert_eq!(
        sent.len(),
        3,
        "initialize, initialized, one model page; no thread"
    );
    assert_eq!(sent[2]["method"], json!("model/list"));
    assert_eq!(state.lock().unwrap().shutdowns, 1);
}

/// The no-prompt handshake against the installed CLI: the factory starts an
/// app-server, completes `initialize`, pages `model/list` and shuts it down.
/// No thread, no prompt, nothing of the account beyond what listing models
/// needs. This is the evidence a newly admitted version stands on until a
/// turn is recorded (0.156.1, 2026-09-25).
#[test]
fn live_codex_no_prompt_handshake_lists_models_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let factory = CodexFactory::new(Arc::new(NoopAttachmentPathResolver));
    let started = Instant::now();
    match crate::providers::ManagedProviderFactory::discover_models(
        &factory,
        "codex".into(),
        directory.path().to_owned(),
    ) {
        Ok(models) => eprintln!(
            "codex_handshake: models={} elapsed_ms={}",
            models.models().len(),
            started.elapsed().as_millis()
        ),
        Err(error) => panic!(
            "codex_handshake failed: stage={:?} diagnostic={:?} code={:?} elapsed_ms={}",
            error.stage,
            error.diagnostic,
            error.code,
            started.elapsed().as_millis()
        ),
    }
}

/// Opt-in: Codex starts an ephemeral thread with the desktop MCP bridge
/// configured beside it, as a chat's session does, and shuts down; no
/// prompt. The evidence is that the app-server accepts the `mcp_servers`
/// overrides and reaches its thread.
#[test]
fn live_codex_starts_a_thread_with_the_desktop_bridge_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let bridge = tree.join(if cfg!(windows) {
        "target/debug/koru-mcp.exe"
    } else {
        "target/debug/koru-mcp"
    });
    if !bridge.is_file() {
        eprintln!("codex_bridge: no koru-mcp built at {}", bridge.display());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let outcome = (|| {
        let executable = crate::providers::resolve_probe_executable("codex".as_ref())?;
        let probe = CodexCompatibilityProbe::new(
            executable.clone(),
            directory.path().to_owned(),
            Arc::new(NoopAttachmentPathResolver),
        );
        let version = probe.version(crate::providers::ProbeLimits::default())?;
        let config = CodexConfig::new(
            executable,
            directory.path().to_owned(),
            version,
            Arc::new(NoopAttachmentPathResolver),
        )?
        .with_mcp_servers(vec![crate::providers::McpServerLaunch {
            name: "koru".into(),
            command: bridge.clone().into_os_string(),
            args: vec!["serve".into(), "--stdio".into()],
            env: Vec::new(),
        }]);
        let mut control = CodexControl::new(config);
        let result = control.start(AgentStart::fresh(SessionId::mint(), None));
        let shutdown = control.shutdown();
        let capabilities = result?;
        shutdown?;
        Ok::<_, ProviderFailure>(capabilities)
    })();
    match outcome {
        Ok(caps) => eprintln!(
            "codex_bridge: version={}, elapsed_ms={}",
            caps.executable_version.as_str(),
            started.elapsed().as_millis()
        ),
        Err(error) => panic!(
            "codex_bridge failed: stage={:?} diagnostic={:?} code={:?} elapsed_ms={}",
            error.stage,
            error.diagnostic,
            error.code,
            started.elapsed().as_millis()
        ),
    }
}

/// Explicit acceptance: does Codex page through the bridge's `tools/list`?
/// Starts the installed Codex app-server with this tree's `koru-mcp` as its
/// `mcp_servers.koru` override, as a chat's session does, then reads the
/// bridge's own log of the `tools/list` requests and cursors it answered
/// (`providers::bridge_log_live`). No prompt. The line is
/// `bridge_pages: provider=codex …`.
#[test]
#[ignore = "starts the installed Codex; explicit acceptance only"]
fn live_codex_pages_through_the_bridge_tool_list() {
    use crate::providers::bridge_log_live;
    assert!(
        koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE),
        "explicit live-provider opt-in required"
    );
    let directory = tempfile::tempdir().unwrap();
    let bridge_root = tempfile::tempdir().unwrap();
    let bridge = bridge_log_live::bridge_launch(bridge_root.path()).expect("build koru-mcp first");
    let started = Instant::now();
    let outcome = (|| {
        let executable = crate::providers::resolve_probe_executable("codex".as_ref())?;
        let probe = CodexCompatibilityProbe::new(
            executable.clone(),
            directory.path().to_owned(),
            Arc::new(NoopAttachmentPathResolver),
        );
        let version = probe.version(crate::providers::ProbeLimits::default())?;
        let config = CodexConfig::new(
            executable,
            directory.path().to_owned(),
            version,
            Arc::new(NoopAttachmentPathResolver),
        )?
        .with_mcp_servers(vec![bridge]);
        let mut control = CodexControl::new(config);
        let result = control.start(AgentStart::fresh(SessionId::mint(), None));
        let log = bridge_log_live::wait_for_the_end(bridge_root.path(), Duration::from_secs(30));
        let shutdown = control.shutdown();
        result?;
        shutdown?;
        Ok::<_, ProviderFailure>(log)
    })();
    match outcome {
        Ok(log) => {
            eprintln!(
                "bridge_pages: provider=codex, {}, elapsed_ms={}",
                log.summary(),
                started.elapsed().as_millis()
            );
            assert!(
                log.reached_the_end() && log.followed_every_cursor(),
                "codex did not page to the end"
            );
        }
        Err(error) => panic!(
            "bridge_pages: provider=codex, start_failed stage={:?} diagnostic={:?}, {}",
            error.stage,
            error.diagnostic,
            bridge_log_live::read(bridge_root.path()).summary()
        ),
    }
}

#[test]
fn steering_adds_text_to_the_running_turn_with_its_native_id_and_only_then() {
    let mut frames = initialize_frames(false);
    frames.push(json!({ "id": 4, "result": { "turn": { "id": "native-turn-1" } } }));
    frames.push(json!({ "id": 5, "result": { "turnId": "native-turn-1" } }));
    let (mut control, state) = replay_control(frames, Arc::new(NoopAttachmentPathResolver));
    control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    let turn_id = AgentTurnId::mint();
    control
        .submit(AgentInput {
            turn_id,
            text: "start".into(),
            attachments: Vec::new(),
        })
        .unwrap();

    control
        .steer(AgentInput {
            turn_id,
            text: "also this".into(),
            attachments: Vec::new(),
        })
        .unwrap();
    let sent = state.lock().unwrap().sent.clone();
    let steer = sent
        .iter()
        .find(|message| message.get("method").and_then(Value::as_str) == Some("turn/steer"))
        .expect("a turn/steer request");
    let params = &steer["params"];
    assert_eq!(params["threadId"], "fixture-thread");
    assert_eq!(params["expectedTurnId"], "native-turn-1");
    assert_eq!(params["input"][0]["text"], "also this");

    // A turn this adapter never started, or one that finished, is not steered.
    let stranger = control
        .steer(AgentInput {
            turn_id: AgentTurnId::mint(),
            text: "x".into(),
            attachments: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(stranger.code, ErrorCode::InvalidTransition);
    control.turns.get_mut(&turn_id).unwrap().terminal = true;
    let finished = control
        .steer(AgentInput {
            turn_id,
            text: "late".into(),
            attachments: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(finished.code, ErrorCode::InvalidTransition);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|message| message.get("method").and_then(Value::as_str) == Some("turn/steer"))
            .count(),
        1,
        "nothing was sent for either refusal"
    );
}
