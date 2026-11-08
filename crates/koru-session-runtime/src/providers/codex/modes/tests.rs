// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn codex_is_offered_three_modes_with_ask_for_approval_current() {
    let modes = offered();
    let ids: Vec<_> = modes.modes().iter().map(|mode| mode.id.as_str()).collect();
    assert_eq!(ids, [ASK, AUTO_REVIEW, FULL_ACCESS]);
    assert_eq!(modes.current().map(|id| id.as_str()), Some(ASK));
}

#[test]
fn each_mode_maps_to_its_approval_sandbox_and_reviewer() {
    let policy = |mode: Option<&str>| {
        let mode = mode.map(|mode| mode.parse().unwrap());
        Policy::of(mode.as_ref())
    };
    let ask = policy(Some(ASK)).unwrap();
    assert_eq!(
        policy(None),
        Some(ask.clone()),
        "no mode is Ask for approval"
    );
    assert_eq!(
        (ask.approval, ask.sandbox, ask.reviewer),
        ("on-request", "read-only", "user")
    );
    assert_eq!(ask.sandbox_policy(), serde_json::json!({"type":"readOnly"}));
    let review = policy(Some(AUTO_REVIEW)).unwrap();
    assert_eq!(
        (review.approval, review.sandbox, review.reviewer),
        ("on-request", "workspace-write", "auto_review")
    );
    assert_eq!(
        review.sandbox_policy(),
        serde_json::json!({"type":"workspaceWrite"})
    );
    let full = policy(Some(FULL_ACCESS)).unwrap();
    assert_eq!(
        (full.approval, full.sandbox, full.reviewer),
        ("never", "danger-full-access", "user")
    );
    assert_eq!(
        full.sandbox_policy(),
        serde_json::json!({"type":"dangerFullAccess"})
    );
    assert_eq!(policy(Some("plan")), None);
}
