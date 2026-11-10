// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::fmt::Write;
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

struct Fields(String);
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).unwrap();
    }
}
thread_local! {
    static ACTIVE_CAPTURE: std::cell::RefCell<Option<Capture>> = const { std::cell::RefCell::new(None) };
}

struct CaptureSubscriber;

fn with_capture(capture: Capture, run: impl FnOnce()) {
    // ACP callsites are shared by parallel tests. Keep their subscriber alive
    // for the process, but retain events only from this test's thread.
    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| {
        tracing::subscriber::set_global_default(CaptureSubscriber)
            .expect("runtime tests own their capture subscriber");
    });
    struct Restore(Option<Capture>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE_CAPTURE.with(|active| *active.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(ACTIVE_CAPTURE.with(|active| active.replace(Some(capture))));
    run();
}

impl Subscriber for CaptureSubscriber {
    // Capture membership is thread-local, so interest must remain dynamic.
    fn register_callsite(&self, _: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::TRACE)
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        ACTIVE_CAPTURE
            .try_with(|active| active.borrow().is_some())
            .unwrap_or(false)
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        ACTIVE_CAPTURE.with(|active| {
            if let Some(capture) = active.borrow().as_ref() {
                capture.0.lock().unwrap().push(fields.0);
            }
        });
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[test]
fn logs_capture_lifecycle_counts_without_provider_content_and_coalesce_streams() {
    let capture = Capture::default();
    with_capture(capture.clone(), || {
        let mut h = harness(FakeAcpConfig::default(), None, None);
        h.control.config.route.provider =
            ProviderKind::from_str("private-provider-sentinel").unwrap();
        h.control.config.delegation = Some(delegation());
        h.control.config.route.session_new_meta =
            Some(json!({"private-meta-sentinel":"private-value-sentinel"}));
        start(&mut h.control);
        let turn = AgentTurnId::mint();
        h.control
            .submit(AgentInput {
                model: None,
                turn_id: turn,
                text: "private-prompt-sentinel".into(),
                attachments: vec![],
                references: Vec::new(),
                mode: None,
                fast: None,
                reasoning: None,
                skills: Vec::new(),
            })
            .unwrap();
        h.control
            .handle_frame(json!({"jsonrpc":"2.0", "method":"private-method-sentinel"}))
            .unwrap();
        for _ in 0..3 {
            h.control.handle_frame(json!({"jsonrpc":"2.0", "method":"session/update", "params":{
                "sessionId":"private-session-sentinel", "update":{"private-update-sentinel":"private-value-sentinel"}
            }})).unwrap();
            h.control.handle_frame(json!({"jsonrpc":"2.0", "method":"session/update", "params":{
                "sessionId":SESSION, "update":{"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"private-response-sentinel"}}
            }})).unwrap();
        }
        h.control.handle_frame(json!({"jsonrpc":"2.0", "method":"session/update", "params":{
            "sessionId":SESSION, "update":{"sessionUpdate":"tool_call", "toolCallId":"private-tool-sentinel", "title":"private-title-sentinel", "kind":"search", "status":"failed"}
        }})).unwrap();
        h.control.handle_frame(json!({"jsonrpc":"2.0", "id":"private-request-sentinel", "method":"session/request_permission", "params":{
            "sessionId":SESSION, "toolCall":{"toolCallId":"private-tool-sentinel", "title":"private-approval-sentinel"},
            "options":[{"optionId":"private-option-sentinel", "kind":"allow_once", "name":"private-choice-sentinel"}]
        }})).unwrap();
        let approval_id = *h.control.approvals.keys().next().unwrap();
        h.control
            .answer_approval(approval_id, ApprovalDecision::Approve, None)
            .unwrap();
        h.control.cancel(turn).unwrap();
        drain(&mut h.control);
        h.control.shutdown().unwrap();

        // Startup errors and deadline/forced terminal outcomes use the same field boundary.
        let mut bad = harness(
            FakeAcpConfig::default(),
            Some(response(1, json!({"private-error-sentinel":true}))),
            None,
        );
        assert!(
            bad.control
                .start(AgentStart::fresh(SessionId::mint(), None))
                .is_err()
        );
        super::super::diagnostics::probe_failure(
            &bad.control.config.route,
            &failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ),
        );
        let mut stopping = harness(
            FakeAcpConfig {
                ignore_cancel: true,
                ..Default::default()
            },
            None,
            None,
        );
        stopping.control.config.cancel_timeout = Duration::ZERO;
        start(&mut stopping.control);
        let turn = AgentTurnId::mint();
        stopping.control.submit(input(turn)).unwrap();
        stopping.control.cancel(turn).unwrap();
        drain(&mut stopping.control);
        stopping.control.shutdown().unwrap();
    });
    let records = capture.0.lock().unwrap();
    let text = records.join("\n");
    for forbidden in [
        "private-",
        "secret-bearer-test",
        "127.0.0.1",
        SESSION,
        PERMISSION_ID,
        "<prompt>",
    ] {
        assert!(
            !text.contains(forbidden),
            "forbidden content in captured trace"
        );
    }
    for event in [
        "acp.starting",
        "acp.ready",
        "acp.turn_started",
        "acp.first_output",
        "acp.tool_started",
        "acp.tool_completed",
        "acp.approval_required",
        "acp.approval_resolved",
        "acp.cancel_requested",
        "acp.turn_finished",
        "acp.shutdown",
        "acp.failed",
        "acp.probe_failed",
        "acp.cancel_deadline",
    ] {
        assert!(text.contains(event), "missing event {event}");
    }
    assert_eq!(
        records
            .iter()
            .filter(|line| line.contains("acp.first_output"))
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|line| line.contains("acp.foreign_session_signal"))
            .count(),
        1
    );
    assert!(text.contains("message_chunks=3"));
    assert!(text.contains("session_foreign_signals=3"));
    assert!(text.contains("failed_tools=1"));
    assert!(text.contains("kind=Some(Other)"));
}

#[test]
fn model_selection_logs_only_koru_identity_and_never_private_model_metadata() {
    let capture = Capture::default();
    with_capture(capture.clone(), || {
        let (mut control, _) = crate::providers::acp::models::tests::selected_control(true, false);
        start(&mut control);
        control.shutdown().unwrap();
    });
    let lines = capture.0.lock().unwrap().join("\n");
    assert!(lines.contains("acp.model.selected"));
    assert!(lines.contains("session_id="));
    assert!(!lines.contains("private-"));
}
