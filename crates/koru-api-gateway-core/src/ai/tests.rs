// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_retry_refusal_is_a_nonretryable_conflict() {
    let failure = Failure {
        code: "routine_retry_unsupported".into(),
        message: "".into(),
        retryable: false,
        details: Default::default(),
    };
    assert_eq!(status_for(&failure), StatusCode::CONFLICT);
    assert!(!failure.retryable);
}

/// `m8_gateway_team_paused` (M8 piece D): the two team codes are client
/// conflicts, never a server error, and neither is retryable as it stands.
#[test]
fn m8_gateway_team_paused_and_revision_conflict_are_conflicts() {
    use koru_ai_core::bots::BotError;
    for (error, code) in [
        (
            BotError::TeamPaused,
            koru_ai_wire::bot_team_codes::TEAM_PAUSED,
        ),
        (
            BotError::TeamRevisionConflict,
            koru_ai_wire::bot_team_codes::TEAM_REVISION_CONFLICT,
        ),
    ] {
        assert_eq!(error.code(), code);
        let failure = Failure {
            code: code.into(),
            message: "The Bot operation could not be completed.".into(),
            retryable: false,
            details: Default::default(),
        };
        assert_eq!(status_for(&failure), StatusCode::CONFLICT, "{code}");
    }
    // A paused Bot's own refusal keeps its code and status (contract 2.3).
    let own = Failure {
        code: BotError::Forbidden.code().into(),
        message: String::new(),
        retryable: false,
        details: Default::default(),
    };
    assert_eq!(status_for(&own), StatusCode::FORBIDDEN);
}
