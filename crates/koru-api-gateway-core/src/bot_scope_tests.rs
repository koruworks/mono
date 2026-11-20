// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn bot_management_requires_an_explicit_vouched_scope() {
    for admission in [Admission::Ordinary, Admission::Vouched] {
        assert!(!effective_scopes(&[], admission).contains(&"bots.manage".into()));
    }
    assert!(effective_scopes(&["bots.manage".into()], Admission::Ordinary).is_empty());
    assert_eq!(
        effective_scopes(&["bots.manage".into()], Admission::Vouched),
        vec!["bots.manage"]
    );
}

#[test]
fn retired_task_and_linked_device_scopes_cannot_be_minted() {
    let retired = [
        "bots.read".into(),
        "bots.run".into(),
        "bots.configure".into(),
    ];
    for admission in [Admission::Ordinary, Admission::Vouched] {
        assert!(effective_scopes(&retired, admission).is_empty());
    }
}

#[test]
fn every_bot_route_requires_vouched_management_authority() {
    let bot_routes: Vec<_> = ai::ROUTES
        .iter()
        .filter(|route| route.path.starts_with("/v1/ai/bots"))
        .collect();
    assert!(!bot_routes.is_empty());
    for route in bot_routes {
        assert_eq!(
            route.scope,
            koru_ai_api::AiScope::ManageBots,
            "{}",
            route.path
        );
        assert!(!route.path.contains("/tasks/"));
        assert_ne!(route.path, "/v1/ai/bots/list");
    }
}
