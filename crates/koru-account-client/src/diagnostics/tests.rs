// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn server_supplied_unknown_tokens_are_not_diagnostic_text() {
    let error = ClientError::Api {
        status: 400,
        code: ErrorCode::parse("credential_sentinel_private").unwrap(),
    };
    let diagnostic = Failure::new(Some(400), &error);
    assert_eq!(diagnostic.code, "unknown_api_error");
    assert!(!format!("{diagnostic:?}").contains("sentinel"));
    assert_eq!(diagnostic.status, Some(400));
}

/// A refused Cloud sync credential says why. Without this a device that was
/// never authorized for a Workspace and one whose Workspace had Cloud off
/// both read `unknown_api_error`, and neither could be told apart from a log.
#[test]
fn a_refused_sync_credential_names_its_reason() {
    for code in [
        "workspace_not_authorized",
        "cloud_disabled",
        "authorization_stale",
    ] {
        let error = ClientError::Api {
            status: 403,
            code: ErrorCode::parse(code).unwrap(),
        };
        assert_eq!(Failure::new(Some(403), &error).code, code);
    }
}

#[test]
fn retry_noise_is_suppressed_per_route_until_failure_changes_or_recovers() {
    let diagnostics = Diagnostics::default();
    let timeout = Some(Failure::new(
        None,
        &ClientError::Transport(TransportError::Timeout),
    ));
    let first = RouteId::GoogleWebStart;
    let other = RouteId::WebExchange;
    assert!(diagnostics.changed(first, timeout));
    assert!(!diagnostics.changed(first, timeout));
    assert!(diagnostics.changed(other, timeout));
    assert!(!diagnostics.changed(first, timeout));
    assert!(diagnostics.changed(first, None));
    assert!(!diagnostics.changed(first, None));
    assert!(diagnostics.changed(first, timeout));
}

#[test]
fn a_refusal_that_is_not_an_envelope_keeps_its_status_and_never_its_body() {
    struct Response;
    impl super::super::Transport for Response {
        fn send(
            &self,
            _: super::super::HttpRequest,
        ) -> Result<super::super::HttpResponse, TransportError> {
            Ok(super::super::HttpResponse {
                status: 502,
                body: b"token_and_email_sentinel".to_vec(),
            })
        }
    }
    let client = super::super::AccountClient::with_transport(std::sync::Arc::new(Response));
    let result: Result<serde_json::Value, _> =
        client.execute(RouteId::GoogleWebStart, vec![], None, false, None);
    // The error value carries the status too, not only the log line: a
    // caller deciding whether a route exists cannot read a `tracing` field.
    assert_eq!(result, Err(ClientError::ApiWithoutCode { status: 502 }));
    let failures = client.diagnostics.failures.lock().unwrap();
    let observed = failures.get(&RouteId::GoogleWebStart).unwrap();
    assert_eq!(observed.status, Some(502));
    assert_eq!(observed.code, "api_without_code");
    assert!(!format!("{observed:?}").contains("sentinel"));
}
