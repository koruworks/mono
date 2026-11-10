// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn bot_management_requires_explicit_agent_vouched_scope() {
    for admission in [Admission::Ordinary, Admission::Vouched] {
        assert!(!effective_scopes(&[], admission).contains(&"bots.manage".into()));
    }
    assert!(
        !effective_scopes(&["bots.manage".into()], Admission::Ordinary)
            .contains(&"bots.manage".into())
    );
    assert!(
        effective_scopes(&["bots.manage".into()], Admission::Vouched)
            .contains(&"bots.manage".into())
    );
}
