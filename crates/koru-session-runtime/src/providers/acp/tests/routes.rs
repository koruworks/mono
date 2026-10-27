// SPDX-License-Identifier: MPL-2.0

use super::*;

fn routes() -> Vec<(AcpRoute, &'static str)> {
    vec![
        (AcpRoute::claude_code(), "0.76.0"),
        (AcpRoute::opencode(), "1.18.27"),
        (AcpRoute::kimi(), "0.42.0"),
        (AcpRoute::grok(), "grok 1.0.25 (f7e67d6988e2) [stable]"),
        // No printed version: the carrier is the executable's SHA-256.
        (
            AcpRoute::antigravity(),
            "04fd51b5c6dfb6d11e649c2cfc61a64ca31d9a4a58a9a3e5c93db934cd7a9695",
        ),
    ]
}

#[test]
fn version_carriers_accept_pinned_output_and_refuse_adornments() {
    for (route, output) in routes() {
        let version = route.parse_version(&format!("{output}\r\n")).unwrap();
        assert_eq!(
            route.supported_versions[0], version,
            "the first pin is the qualified one"
        );
        for invalid in [
            "",
            "0.0.1",
            &format!("prefix {output}"),
            &format!("{output} trailing"),
        ] {
            let error = route.parse_version(invalid).unwrap_err();
            assert_eq!(error.stage, ProviderStage::Version);
            assert_eq!(error.diagnostic, ProviderDiagnostic::VersionRejected);
        }
    }
    // The versions admitted on the maintainers' instruction after their no-prompt
    // handshake passed (AGENTUX, 2026-09-25) parse to themselves.
    assert_eq!(
        AcpRoute::kimi().parse_version("2.0.0\n").unwrap().as_str(),
        "2.0.0"
    );
    assert_eq!(
        AcpRoute::grok()
            .parse_version("grok 1.0.34 (3736acbc8658) [stable]\n")
            .unwrap()
            .as_str(),
        "1.0.34"
    );
}

/// **A release at or above the qualified pin is admitted** (the CTO's note,
/// 2026-10-26): the installed CLIs had moved past the pins. The pin stays
/// the floor, an adorned print is still refused, and Antigravity — known by
/// its digest, not a number — admits nothing it was not pinned to.
#[test]
fn a_release_above_the_pin_is_admitted_and_one_below_it_is_not() {
    assert_eq!(
        AcpRoute::kimi().parse_version("2.0.1").unwrap().as_str(),
        "2.0.1"
    );
    assert_eq!(
        AcpRoute::kimi().parse_version("v2.3.0\n").unwrap().as_str(),
        "2.3.0"
    );
    assert!(
        AcpRoute::kimi().parse_version("0.41.9").is_err(),
        "below the pin"
    );
    assert_eq!(
        AcpRoute::claude_code()
            .parse_version("0.77.2")
            .unwrap()
            .as_str(),
        "0.77.2"
    );
    assert!(AcpRoute::claude_code().parse_version("0.75.9").is_err());
    assert_eq!(
        AcpRoute::opencode()
            .parse_version("1.19.0")
            .unwrap()
            .as_str(),
        "1.19.0"
    );
    assert_eq!(
        AcpRoute::grok()
            .parse_version("grok 1.0.40 (0123456789ab) [stable]")
            .unwrap()
            .as_str(),
        "1.0.40"
    );
    assert_eq!(
        AcpRoute::grok()
            .parse_version("grok 1.0.25 (other) [stable]")
            .unwrap()
            .as_str(),
        "1.0.25",
        "the hash is not the version"
    );
    assert!(
        AcpRoute::grok()
            .parse_version("grok 1.0.24 (x) [stable]")
            .is_err()
    );
    assert!(
        AcpRoute::grok().parse_version("1.0.40").is_err(),
        "not grok's print"
    );
    assert!(AcpRoute::antigravity().parse_version("1.2.2").is_err());
    let route = AcpRoute::kimi();
    assert!(route.supports(&ProviderVersion::from_str("2.0.0").unwrap()));
    assert!(route.supports(&ProviderVersion::from_str("3.0").unwrap()));
    assert!(!route.supports(&ProviderVersion::from_str("0.1.0").unwrap()));
    assert!(!route.supports(&ProviderVersion::from_str("beta").unwrap()));
}

#[test]
fn process_policy_cannot_be_overridden_by_caller_case_or_duplicate_environment() {
    for (mut route, key, expected) in [
        (AcpRoute::grok(), "GROK_DISABLE_AUTOUPDATER", "1"),
        (AcpRoute::grok(), "GROK_SUBAGENTS", "0"),
    ] {
        route
            .environment
            .push((key.to_ascii_lowercase().into(), "wrong".into()));
        route
            .environment
            .push(("UNRELATED".into(), "preserved".into()));
        route.enforce_environment();
        let entries: Vec<_> = route
            .environment
            .iter()
            .filter(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case(key))
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, std::ffi::OsString::from(expected));
        assert!(
            route
                .environment
                .contains(&("UNRELATED".into(), "preserved".into()))
        );
    }
}

#[cfg(windows)]
#[test]
fn named_batch_launches_append_only_the_reviewed_fixed_tokens() {
    for (mut route, _) in routes() {
        let executable = std::ffi::OsStr::new(r"C:\fixture with spaces\agent.cmd");
        let fixed = route.arguments.clone();
        let rendered = koru_session_profiles::platform_launch_argv(executable.into(), &[]).unwrap();
        let (program, arguments) = route.launch(executable, false).unwrap();
        assert_eq!(program, rendered[0]);
        let mut expected = rendered[1..].to_vec();
        expected.extend(fixed);
        assert_eq!(arguments, expected);
        let (_, version) = route.launch(executable, true).unwrap();
        let mut expected = rendered[1..].to_vec();
        expected.push("--version".into());
        assert_eq!(version, expected);
        route.arguments.push("authored & command".into());
        assert!(route.launch(executable, false).is_err());
        assert!(route.launch(executable, true).is_err());
    }
}

#[test]
fn experimental_routes_accept_delegation_without_claiming_native_child_control() {
    for route in [AcpRoute::opencode(), AcpRoute::kimi()] {
        let mut h = harness(FakeAcpConfig::default(), None, None);
        h.control.config.executable_version = route.supported_versions[0].clone();
        h.control.config.route = route;
        h.control.config.delegation = Some(delegation());
        let caps = start(&mut h.control);
        assert_eq!(
            caps.delegation.child_sessions,
            ChildSessionPolicy::Experimental
        );
        assert!(DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation).delegatable());
        let state = h.state.lock().unwrap();
        let session = state
            .sent()
            .iter()
            .find(|frame| frame["method"] == "session/new")
            .unwrap();
        assert_eq!(session["params"]["mcpServers"][0]["type"], "http");
        assert_eq!(
            session["params"]["mcpServers"][0]["url"],
            delegation().mcp_endpoint.as_str()
        );
        assert_eq!(
            session["params"]["mcpServers"][0]["headers"][0]["name"],
            "Authorization"
        );
    }
}

#[test]
fn experimental_routes_still_require_http_protocol_and_a_version_at_the_pin() {
    for route in [AcpRoute::opencode(), AcpRoute::kimi()] {
        for (http, protocol) in [(false, 1), (true, 2)] {
            let mut h = harness(
                FakeAcpConfig::default(),
                Some(initialized(json!(http), json!(protocol))),
                None,
            );
            h.control.config.executable_version = route.supported_versions[0].clone();
            h.control.config.route = route.clone();
            h.control.config.delegation = Some(delegation());
            assert!(
                h.control
                    .start(AgentStart::fresh(SessionId::mint(), None))
                    .is_err()
            );
            assert!(
                !h.state
                    .lock()
                    .unwrap()
                    .sent()
                    .iter()
                    .any(|frame| frame["method"] == "session/new")
            );
        }
        // A release below the pin: refused before anything is sent.
        let mut h = harness(FakeAcpConfig::default(), None, None);
        h.control.config.executable_version = ProviderVersion::from_str("0.0.1").unwrap();
        h.control.config.route = route;
        h.control.config.delegation = Some(delegation());
        let error = h
            .control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .unwrap_err();
        assert_eq!(error.diagnostic, ProviderDiagnostic::VersionRejected);
        assert!(h.state.lock().unwrap().sent().is_empty());
    }
}

#[test]
fn uncontrolled_generic_route_remains_blocked_before_sending_endpoint() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.route.child_sessions = ChildSessionPolicy::Uncontrolled;
    let caps = start(&mut h.control);
    assert_eq!(
        DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation).missing(),
        vec![DelegationRequirement::Cancel]
    );
    let mut fresh = harness(FakeAcpConfig::default(), None, None);
    fresh.control.config.route.child_sessions = ChildSessionPolicy::Uncontrolled;
    fresh.control.config.delegation = Some(delegation());
    assert_eq!(
        fresh
            .control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityNotNegotiated
    );
    assert!(fresh.state.lock().unwrap().sent().is_empty());
}

fn recorded_routes() -> Vec<(
    AcpRoute,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
)> {
    vec![
        (
            AcpRoute::opencode(),
            include_str!("fixtures/opencode-1.18.27/version.txt"),
            include_str!("fixtures/opencode-1.18.27/initialize.json"),
            include_str!("fixtures/opencode-1.18.27/new-session.json"),
            include_str!("fixtures/opencode-1.18.27/auth-required.json"),
        ),
        (
            AcpRoute::kimi(),
            include_str!("fixtures/kimi-0.42.0/version.txt"),
            include_str!("fixtures/kimi-0.42.0/initialize.json"),
            include_str!("fixtures/kimi-0.42.0/new-session.json"),
            include_str!("fixtures/kimi-0.42.0/auth-required.json"),
        ),
        (
            AcpRoute::grok(),
            include_str!("fixtures/grok-1.0.25/version.txt"),
            include_str!("fixtures/grok-1.0.25/initialize.json"),
            include_str!("fixtures/grok-1.0.25/new-session.json"),
            include_str!("fixtures/grok-1.0.25/auth-required.json"),
        ),
        (
            AcpRoute::antigravity(),
            include_str!("fixtures/antigravity-1.2.1/digest.txt"),
            include_str!("fixtures/antigravity-1.2.1/initialize.json"),
            include_str!("fixtures/antigravity-1.2.1/new-session.json"),
            include_str!("fixtures/antigravity-1.2.1/auth-required.json"),
        ),
    ]
}

#[test]
fn recorded_initialization_and_auth_failures_pass_the_real_route_parser_without_native_content() {
    for (route, version, init, session, auth_error) in recorded_routes() {
        for fail_auth in [false, true] {
            let mut h = harness(FakeAcpConfig::default(), None, None);
            h.control.config.executable_version = route.parse_version(version).unwrap();
            h.control.config.route = route.clone();
            let mut frames = h.injected.lock().unwrap();
            frames.push_back(serde_json::from_str(init).unwrap());
            if fail_auth {
                frames.push_back(serde_json::from_str(auth_error).unwrap());
            } else {
                if route.cached_auth_method.is_some() {
                    frames.push_back(response(2, json!({})));
                }
                frames.push_back(serde_json::from_str(session).unwrap());
                if route.is_grok() {
                    frames.push_back(grok_inventory("<private-session>", json!(["read_file"])));
                }
            }
            drop(frames);
            let result = h.control.start(AgentStart::fresh(SessionId::mint(), None));
            if fail_auth {
                let error = result.unwrap_err();
                assert_eq!(error.diagnostic, ProviderDiagnostic::AuthenticationRequired);
                assert!(!format!("{error:?}").contains("private-auth"));
            } else {
                let caps = result.unwrap();
                assert_eq!(caps.provider, route.provider);
                assert_eq!(caps.delegation.mcp, McpSupport::Supported);
                assert!(!format!("{caps:?}").contains("private-session"));
                if route.cached_auth_method.is_some() {
                    let state = h.state.lock().unwrap();
                    let auth = state
                        .sent()
                        .iter()
                        .find(|frame| frame["method"] == "authenticate")
                        .unwrap();
                    assert_eq!(auth["params"]["methodId"], "cached_token");
                    assert_eq!(auth["params"]["_meta"]["headless"], true);
                }
            }
        }
    }
}

fn grok_inventory(session: &str, tools: Value) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":session,"update":{"sessionUpdate":"available_commands_update",
        "availableCommands":[],"_meta":{"tools":tools}}}})
}

fn grok_harness(before_reply: bool, tools: Option<Value>) -> Harness {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.route = AcpRoute::grok();
    h.control.config.executable_version = h.control.config.route.supported_versions[0].clone();
    let mut frames = h.injected.lock().unwrap();
    frames.push_back(response(
        1,
        json!({"protocolVersion":1,
        "agentCapabilities":{"mcpCapabilities":{"http":true}},
        "authMethods":[{"id":"cached_token","name":"<method>"}]}),
    ));
    frames.push_back(response(2, json!({})));
    if before_reply && let Some(tools) = &tools {
        frames.push_back(grok_inventory(SESSION, tools.clone()));
    }
    frames.push_back(response(3, json!({"sessionId":SESSION})));
    if !before_reply && let Some(tools) = tools {
        frames.push_back(grok_inventory(SESSION, tools));
    }
    drop(frames);
    h
}

#[test]
fn grok_requires_a_complete_session_bound_inventory_before_accepting_start() {
    for before_reply in [false, true] {
        let mut h = grok_harness(
            before_reply,
            Some(json!(["read_file", "get_command_or_subagent_output"])),
        );
        let caps = start(&mut h.control);
        assert_eq!(caps.delegation.child_sessions, ChildSessionPolicy::Disabled);
        assert!(
            DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation)
                .missing()
                .is_empty()
        );
        assert!(!format!("{caps:?}").contains(SESSION));
    }
    for tools in [
        None,
        Some(json!([])),
        Some(json!(null)),
        Some(json!([3])),
        Some(json!(["spawn_subagent"])),
        Some(json!(["task"])),
        Some(json!(["Task"])),
    ] {
        let mut h = grok_harness(false, tools);
        assert!(
            h.control
                .start(AgentStart::fresh(SessionId::mint(), None))
                .is_err()
        );
        assert!(h.control.shut_down);
        assert!(h.control.capabilities().is_none());
    }
}

#[test]
fn grok_ignores_other_sessions_but_stops_on_later_child_reintroduction_once() {
    let mut h = grok_harness(true, Some(json!(["read_file"])));
    start(&mut h.control);
    drain(&mut h.control);
    h.control
        .handle_frame(grok_inventory("other-session", json!(["Task"])))
        .unwrap();
    assert!(!h.control.shut_down);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    h.injected
        .lock()
        .unwrap()
        .push_back(grok_inventory(SESSION, json!(["read_file", "task"])));
    let events = drain(&mut h.control);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event,
        AgentEvent::TurnFailed{turn_id,..} if *turn_id == turn))
            .count(),
        1
    );
    assert!(h.control.shut_down);
    assert!(h.control.next_event(Duration::ZERO).is_err());
    assert!(h.control.submit(input(AgentTurnId::mint())).is_err());
    assert!(!format!("{events:?}").contains(SESSION));
    assert!(!format!("{events:?}").contains("read_file"));
}

#[test]
fn grok_wrong_startup_session_and_wrong_metadata_carrier_cannot_satisfy_guard() {
    let mut h = grok_harness(false, None);
    h.injected
        .lock()
        .unwrap()
        .push_back(grok_inventory("other-session", json!(["read_file"])));
    assert!(
        h.control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .is_err()
    );
    let mut h = grok_harness(false, None);
    h.injected
        .lock()
        .unwrap()
        .push_back(json!({"jsonrpc":"2.0","method":"session/update",
        "params":{"sessionId":SESSION,"_meta":{"tools":["read_file"]},
        "update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}}));
    assert!(
        h.control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .is_err()
    );
}
