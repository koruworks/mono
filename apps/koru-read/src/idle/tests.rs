// SPDX-License-Identifier: MPL-2.0

use super::*;

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn a_minute_with_no_job_ends_the_process() {
    let policy = IdlePolicy {
        exit_after: IdlePolicy::DEFAULT_EXIT,
    };
    assert_eq!(IdlePolicy::DEFAULT_EXIT, MINUTE);
    assert_eq!(policy.decide(Some(MINUTE)), IdleAction::Exit);
    assert_eq!(
        policy.decide(Some(Duration::from_secs(45))),
        IdleAction::Wait(Duration::from_secs(15))
    );
}

#[test]
fn a_running_job_is_never_idle() {
    let policy = IdlePolicy {
        exit_after: Duration::from_secs(5),
    };
    assert_eq!(
        policy.decide(None),
        IdleAction::Wait(Duration::from_secs(5))
    );
}

#[test]
fn zero_never_exits_and_never_spins() {
    let never = IdlePolicy {
        exit_after: Duration::ZERO,
    };
    assert!(matches!(
        never.decide(Some(Duration::from_secs(86_400))),
        IdleAction::Wait(wait) if wait > Duration::ZERO
    ));
    let soon = IdlePolicy { exit_after: MINUTE };
    assert!(matches!(
        soon.decide(Some(MINUTE - Duration::from_nanos(1))),
        IdleAction::Wait(wait) if wait > Duration::ZERO
    ));
}

#[test]
fn the_variable_is_whole_seconds_and_a_bad_value_is_ignored() {
    assert_eq!(parse_seconds(Some("90"), MINUTE), Duration::from_secs(90));
    assert_eq!(parse_seconds(Some(" 0 "), MINUTE), Duration::ZERO);
    assert_eq!(parse_seconds(Some("1m"), MINUTE), MINUTE);
    assert_eq!(parse_seconds(Some(""), MINUTE), MINUTE);
    assert_eq!(parse_seconds(None, MINUTE), MINUTE);
}
