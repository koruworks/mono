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

#[test]
fn routine_owner_setup_uses_only_the_existing_vouched_management_route() {
    let route = ai::ROUTES
        .iter()
        .find(|route| route.path == "/v1/ai/bots")
        .unwrap();
    assert_eq!(route.scope, koru_ai_api::AiScope::ManageBots);
    for (scopes, admission) in [
        (vec!["bots.manage".into()], Admission::Ordinary),
        (vec!["bots.run".into()], Admission::Vouched),
        (vec![], Admission::Vouched),
    ] {
        assert!(!effective_scopes(&scopes, admission).contains(&"bots.manage".into()));
    }
    let public = public_openapi().to_string();
    for private in [
        "PrepareOccurrence",
        "RoutineBridgeAuthority",
        "FinalizeOccurrence",
    ] {
        assert!(!public.contains(private));
    }
}

/// P4.3: the owner controls of a Routine ride the one existing vouched
/// management route, as commands of the public Bot contract. No route of
/// their own, no ordinary or run scope, and nothing of the private delegate
/// control (its route identity, its Test minute) is public.
#[test]
fn routine_owner_controls_use_only_the_existing_vouched_management_route() {
    let bots: Vec<_> = ai::ROUTES
        .iter()
        .filter(|route| route.path.contains("routine"))
        .collect();
    assert!(bots.is_empty(), "a Routine control has no route of its own");
    let route = ai::ROUTES
        .iter()
        .find(|route| route.path == "/v1/ai/bots")
        .unwrap();
    assert_eq!(route.scope, koru_ai_api::AiScope::ManageBots);
    let public = public_openapi().to_string();
    for command in [
        "set_routine_lifecycle",
        "test_routine",
        "edit_routine",
        "update_routine_access",
        "routine_control",
        "routine_test",
        "routine_access",
        "this_computer",
        "routines_needing_access",
        "routines_still_on",
    ] {
        assert!(public.contains(command), "{command}");
    }
    for private in [
        "ControlRoutine",
        "control_routine",
        "RoutineTestAccepted",
        "RoutineRouteIdentity",
        "routine_bot_paused",
    ] {
        assert!(!public.contains(private), "{private}");
    }
    // Each command decodes on the route's own payload type, closed.
    let bot_id = koru_ai_core::bots::BotId::mint();
    let schedule_id = koru_delegate_core::ScheduleId::mint();
    for body in [
        serde_json::json!({"command":"set_routine_lifecycle","bot_id":bot_id,"schedule_id":schedule_id,"expected_definition_revision":1,"action":"pause"}),
        serde_json::json!({"command":"test_routine","bot_id":bot_id,"schedule_id":schedule_id,"expected_definition_revision":1}),
        serde_json::json!({"command":"edit_routine","bot_id":bot_id,"schedule_id":schedule_id,"expected_definition_revision":1,"expected_name":"Before","name":"After","recurrence":{"type":"floating","cron":"0 9 * * *"}}),
        serde_json::json!({"command":"update_routine_access","request_id":koru_ai_core::bots::BotRequestId::mint(),"bot_id":bot_id,"schedule_id":schedule_id,"expected_grant_revision":1,"access":"default","selected_pages":[]}),
    ] {
        let command: koru_ai_wire::BotCommand = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(command.routine_control(), Some((bot_id, schedule_id)));
        let mut owned = body;
        owned["owner_device"] = serde_json::json!("00000000-0000-4000-8000-000000000001");
        assert!(serde_json::from_value::<koru_ai_wire::BotCommand>(owned).is_err());
    }
}

/// `m8_gateway_team_paused` (M8 piece D): pausing and resuming the team are
/// the person's controls. The three commands are commands of the public Bot
/// contract and ride the one vouched management route; the scopes an
/// external client can hold (`bots.read`, `bots.run`) do not reach it, and
/// none of the four external task routes carries a Bot command.
#[test]
fn m8_gateway_team_paused_team_commands_ride_only_the_vouched_management_route() {
    use koru_ai_core::bots::BotRequestId;
    let commands = [
        serde_json::to_value(koru_ai_wire::BotCommand::PauseTeam {
            request_id: BotRequestId::mint(),
            expected_team_revision: 0,
        })
        .unwrap(),
        serde_json::to_value(koru_ai_wire::BotCommand::ResumeTeam {
            request_id: BotRequestId::mint(),
            expected_team_revision: 1,
        })
        .unwrap(),
        serde_json::to_value(koru_ai_wire::BotCommand::ReadTeamPause {}).unwrap(),
    ];
    let public = public_openapi().to_string();
    for command in &commands {
        let name = command["command"].as_str().expect("a tagged command");
        assert!(public.contains(name), "{name} is in the public contract");
    }
    let management = ai::ROUTES
        .iter()
        .find(|route| route.path == "/v1/ai/bots")
        .unwrap();
    assert_eq!(management.scope, koru_ai_api::AiScope::ManageBots);
    // What an external MCP client asks for never yields the management scope.
    for admission in [Admission::Ordinary, Admission::Vouched] {
        let held = effective_scopes(
            &[
                "bots.read".into(),
                "bots.run".into(),
                "store.read_documents".into(),
            ],
            admission,
        );
        assert!(!held.contains(&"bots.manage".into()));
    }
    // The external task routes are exactly four, and none is the Bots route.
    let external: Vec<_> = ai::ROUTES
        .iter()
        .filter(|route| {
            matches!(
                route.scope,
                koru_ai_api::AiScope::ReadBots | koru_ai_api::AiScope::RunBots
            )
        })
        .map(|route| route.operation_id)
        .collect();
    assert_eq!(
        external,
        [
            "ai_list_bots",
            "ai_start_bot_task",
            "ai_read_bot_task",
            "ai_stop_bot_task"
        ]
    );
}
