// SPDX-License-Identifier: MPL-2.0
use super::steer_failure;
use koru_ai_wire::Failure;

fn failure(code: &str, retryable: bool) -> Failure {
    Failure {
        code: code.into(),
        message: "Sanitized transport detail.".into(),
        retryable,
        details: Default::default(),
    }
}

#[test]
fn only_definite_unavailable_steers_allow_automatic_new_send() {
    for code in [
        "ai_network",
        "ai_timeout",
        "unclassified_transport",
        "steer_failed",
        "ai_worker_saturated",
    ] {
        let result = steer_failure(failure(code, true));
        assert_eq!(result.code, "steer_failed");
        assert!(!result.retryable);
        assert_eq!(result.message, "Sanitized transport detail.");
    }
    let safe = steer_failure(failure("steer_unavailable", true));
    assert_eq!(safe.code, "steer_unavailable");
    assert!(safe.retryable);
    assert_eq!(safe.message, "Sanitized transport detail.");
    let mut server = failure("steer_failed", false);
    server.message = "Native delivery is uncertain; review the conversation.".into();
    let expected = server.message.clone();
    let result = steer_failure(server);
    assert!(!result.retryable);
    assert_eq!(result.message, expected);
}
