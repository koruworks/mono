// SPDX-License-Identifier: MPL-2.0

use super::*;

/// D7's throttle: a refused model asks for a re-probe once per window,
/// shared with the failed-readiness re-probe, per agent.
#[test]
fn a_refused_model_re_probes_its_agent_at_most_once_per_window() {
    let opencode: ProviderKind = "opencode".parse().unwrap();
    let claude: ProviderKind = "claude".parse().unwrap();
    let now = Instant::now();
    let mut refreshed = HashMap::new();
    assert!(refresh_due(&refreshed, &opencode, now), "never asked");
    refreshed.insert(opencode.clone(), now);
    assert!(!refresh_due(&refreshed, &opencode, now));
    assert!(!refresh_due(
        &refreshed,
        &opencode,
        now + REFRESH_WINDOW - Duration::from_millis(1)
    ));
    assert!(refresh_due(&refreshed, &opencode, now + REFRESH_WINDOW));
    assert!(
        refresh_due(&refreshed, &claude, now),
        "another agent's own window"
    );
}

/// A failed check and a missing program are looked at again — an agent
/// installed while Koru runs appears without a restart — and a settled
/// answer is not.
#[test]
fn failed_and_missing_rows_are_looked_at_again() {
    assert!(looks_again(&ExecutableAvailability::Missing));
    assert!(looks_again(&ExecutableAvailability::Failed {
        code: ErrorCode::ProviderFailed,
    }));
    for settled in [
        ExecutableAvailability::Available { version: None },
        ExecutableAvailability::Checking,
        ExecutableAvailability::SignedOut {
            code: ErrorCode::ProviderFailed,
        },
    ] {
        assert!(!looks_again(&settled), "{settled:?}");
    }
}

/// D19: a start refused for its level reads as the level, not the model,
/// and its refresh says so.
#[test]
fn a_refused_level_reads_as_the_level_and_refreshes_for_it() {
    let refused = |message: &str| {
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProviderModelUnavailable,
            message: message.into(),
        })
    };
    let level = refused(koru_session_protocol::REASONING_REJECTED);
    assert_eq!(
        session_error("create", level).public_message,
        koru_ai_core::AGENT_REASONING_REJECTED
    );
    assert_eq!(
        refusal_reason(&refused(koru_session_protocol::REASONING_REJECTED)),
        "reasoning"
    );
    let model = refused("provider_model_unavailable");
    assert_eq!(refusal_reason(&model), "model");
    assert_eq!(
        session_error("create", model).public_message,
        "The agent refused that model."
    );
}
