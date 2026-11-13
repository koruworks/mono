// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn mcp_failure_reads_only_closed_internal_tool_code_and_bounded_timing() {
    let line = "WARN tool failed tool=\"mini_app_tool\" code=\"invalid_request\" elapsed_ms=30000.25 error=\"SECRET_SENTINEL\"";
    assert_eq!(
        parse(line, "codex"),
        Some(Record::McpFailed {
            code: "invalid_request",
            elapsed_ms: 30000,
            reason: None,
        })
    );
    for bad in [
        line.replace("mini_app_tool", "foreign_tool"),
        line.replace("invalid_request", "SECRET_SENTINEL"),
        line.replace("30000.25", "NaN"),
        line.replace("30000.25", "600001"),
        format!("{line} code=forbidden"),
    ] {
        assert!(parse(&bad, "codex").is_none());
    }
    assert!(filename("koru-mcp.log.2026-11-13"));
    assert!(!filename("koru-mcp.log.private"));
    assert!(!filename("koru-mcp.log.2026-11-13.part"));
    assert!(!format!("{:?}", parse(line, "codex")).contains("SECRET_SENTINEL"));
}

#[test]
fn mcp_validation_logs_publish_only_exact_closed_leading_categories() {
    let line = "WARN tool failed tool=mini_app_tool code=ai_provider elapsed_ms=6 error=\"mini_app_validation_unknown_field: SECRET_SENTINEL\"";
    let record = parse(line, "codex");
    assert_eq!(
        record,
        Some(Record::McpFailed {
            code: "ai_provider",
            elapsed_ms: 6,
            reason: Some("mini_app_validation_unknown_field")
        })
    );
    assert!(!format!("{record:?}").contains("SECRET_SENTINEL"));
    // The producer logs error = %e.message (Display), without JSON quoting.
    let display = line
        .replace("error=\"", "error=")
        .replace("SENTINEL\"", "SENTINEL");
    assert_eq!(parse(&display, "codex"), record);
    for bad in [
        line.replace("mini_app_validation_unknown_field:", "SECRET_SENTINEL:"),
        line.replace("error=\"", "error=\"prefix "),
        line.replace("unknown_field:", "unknown_field:SECRET_SENTINEL"),
        line.replace(" error=", " private_error="),
        format!("{line} error=\"mini_app_validation_shape: extra\""),
    ] {
        assert!(matches!(
            parse(&bad, "codex"),
            Some(Record::McpFailed { reason: None, .. })
        ));
    }
}

#[test]
fn unknown_session_codes_are_withheld_from_all_diagnostics() {
    assert_eq!(
        super::super::safe_session_code(&ErrorCode::Unknown("private-data".into())),
        "unknown_code_withheld"
    );
    assert_eq!(
        super::super::safe_session_code(&ErrorCode::ProviderFailed),
        "provider_failed"
    );
}

#[test]
fn launch_event_reconstructs_only_known_fields() {
    let line = "2026-11-13T00:00:00Z INFO managed terminal session launch failed event=\"terminal.session.launch_failed\" mode=\"managed\" provider=\"codex\" code=\"provider_incompatible\" stage=Initialize diagnostic=HandshakeRejected os_code=0 ignored=private-data";
    assert_eq!(
        parse(line, "codex"),
        Some(Record::Launch {
            code: ErrorCode::ProviderIncompatible,
            stage: "Initialize",
            diagnostic: "HandshakeRejected",
            os_code: 0,
        })
    );
    assert!(parse(line, "claude").is_none());
    assert!(parse(&line.replace("HandshakeRejected", "private-data"), "codex").is_none());
    assert!(
        parse(
            &line.replace("provider_incompatible", "private-data"),
            "codex"
        )
        .is_none()
    );
    assert!(parse(&format!("{line} code=provider_failed"), "codex").is_none());
}

#[test]
fn acp_fields_are_closed_and_session_prefix_never_needs_output() {
    assert_eq!(
        parse(
            "event=acp.starting provider=claude session_id=Some(session(abcdef12))",
            "claude"
        ),
        Some(Record::AcpStart(LogSession::Prefix(0xabcdef12)))
    );
    assert!(matches!(
        parse(
            "event=acp.failed provider=claude operation=transport stage=Read diagnostic=UnexpectedExit code=provider_failed",
            "claude"
        ),
        Some(Record::AcpFailed {
            operation: "transport",
            ..
        })
    ));
    assert!(parse("event=acp.failed provider=claude operation=private-data stage=Read diagnostic=UnexpectedExit code=provider_failed", "claude").is_none());
    let line = "event=acp.turn_finished session_id=Some(session(abcdef12)) outcome=failed error_code=provider_failed message_chunks=0 tools=1 failed_tools=1 approvals=0 session_unknown_events=0 session_foreign_signals=0 session_queue_high_water=2";
    assert_eq!(
        parse(line, "claude"),
        Some(Record::AcpTurn {
            session: LogSession::Prefix(0xabcdef12),
            outcome: "failed",
            code: Some(ErrorCode::ProviderFailed),
            counts: [0, 1, 1, 0, 0, 0, 2]
        })
    );
    assert!(parse(line, "codex").is_none());
    assert!(
        parse(
            &line.replace("outcome=failed", "outcome=private-data"),
            "claude"
        )
        .is_none()
    );
    assert!(
        parse(
            &line.replace("tools=1", "tools=18446744073709551616"),
            "claude"
        )
        .is_none()
    );
}

#[test]
fn correlates_actual_production_session_debug_format_without_printing_it() {
    let session = SessionId::mint();
    let start = format!(
        "event=acp.starting provider=claude session_id={:?}",
        Some(session)
    );
    let turn = format!(
        "event=acp.turn_finished session_id={:?} outcome=failed error_code=provider_failed message_chunks=0 tools=0 failed_tools=0 approvals=0 session_unknown_events=0 session_foreign_signals=0 session_queue_high_water=1",
        Some(session)
    );
    let Some(Record::AcpStart(start_key)) = parse(&start, "claude") else {
        panic!("production session debug start was not recognized");
    };
    let Some(Record::AcpTurn {
        session: turn_key, ..
    }) = parse(&turn, "claude")
    else {
        panic!("production session debug turn was not recognized");
    };
    assert!(
        start_key == turn_key,
        "production-formatted session keys did not correlate"
    );
    assert!(matches!(start_key, LogSession::Full(actual) if actual == session));
    assert!(
        parse(
            &start.replace(&session.to_string(), "not-a-valid-session-identity"),
            "claude"
        )
        .is_none()
    );
}

#[test]
fn ignores_unrelated_or_oversized_records_and_non_log_names() {
    assert!(parse("event=other-event text=private-data", "codex").is_none());
    assert!(parse(&"x".repeat(MAX_LINE + 1), "codex").is_none());
    assert!(filename("koru-session-host.log.2026-11-13"));
    assert!(!filename("koru-session-host.log.2026-11-13.part"));
    assert!(!filename("koru-session-host.log.private-data"));
    assert!(!filename("other.log.2026-11-13"));
}
