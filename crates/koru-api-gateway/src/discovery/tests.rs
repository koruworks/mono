// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn bot_only_request_requires_agent_vouch_validation() {
    assert!(asks_vouched_scope(&["bots.manage".into()]));
    assert!(asks_vouched_scope(&[WORKSPACE_DESTROY_SCOPE.into()]));
    assert!(asks_vouched_scope(&[WORKSPACE_MIGRATE_SCOPE.into()]));
    assert!(!asks_vouched_scope(&[]));
    assert!(!asks_vouched_scope(&[
        "ai.chat".into(),
        "bots.manage.lookalike".into()
    ]));
}
