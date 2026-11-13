// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn bot_tool_scopes_are_narrow_and_explicit() {
    for scope in ["bots.read", "bots.run"] {
        assert!(!effective_scopes(&[], Admission::Ordinary).contains(&scope.into()));
        assert_eq!(
            effective_scopes(&[scope.into()], Admission::Ordinary),
            vec![scope.to_owned()]
        );
    }
    assert_eq!(
        effective_scopes(
            &["bots.read".into(), "bots.run".into(), "bots.manage".into()],
            Admission::Ordinary
        ),
        vec!["bots.read", "bots.run"]
    );
}
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
