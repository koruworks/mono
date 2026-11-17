// SPDX-License-Identifier: MPL-2.0

//! IPC 20: the refusals of the team pause. Each has one fixed text that
//! interpolates nothing; none is retryable.

/// The `details` key of `bot_team_stale` that carries the epoch the delegate
/// holds. The AI owner writes its row at that epoch plus one and sends again.
pub const TEAM_HELD_EPOCH: &str = "held_epoch";

const TEAM_FAILURES: [(&str, &str); 3] = [
    (
        "bot_team_paused",
        "The team is paused. New work is not admitted.",
    ),
    (
        "bot_team_stale",
        "The team value is older than the one the delegate holds.",
    ),
    (
        "bot_team_conflict",
        "A different team value is already held at this epoch.",
    ),
];

/// The fixed failure of one of the team codes; `None` for every other code.
pub fn team_failure(code: &str) -> Option<crate::Failure> {
    TEAM_FAILURES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(known, message)| crate::Failure::new(*known, *message, false))
}

/// `bot_team_stale` with the epoch the delegate holds in its details.
pub fn team_stale(held_epoch: u64) -> crate::Failure {
    team_failure("bot_team_stale")
        .expect("a team code")
        .with_detail(TEAM_HELD_EPOCH, held_epoch)
}

/// The held epoch a `bot_team_stale` failure carries; `None` for any other
/// failure and for one without a readable epoch.
pub fn team_held_epoch(failure: &crate::Failure) -> Option<u64> {
    if failure.code != "bot_team_stale" {
        return None;
    }
    failure
        .details
        .get(TEAM_HELD_EPOCH)
        .and_then(serde_json::Value::as_u64)
}
