// SPDX-License-Identifier: MPL-2.0

//! Devin CLI 3000.10.48 as `devin acp`, replayed from sanitized fixtures
//! with the notifications it sends around them.
use super::*;

const VERSION: &str = include_str!("fixtures/devin-3000.10.48/version.txt");
const INITIALIZE: &str = include_str!("fixtures/devin-3000.10.48/initialize.json");
const NEW_SESSION: &str = include_str!("fixtures/devin-3000.10.48/new-session.json");

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}

/// The frames in the order the signed-in session sent them: its own MCP
/// notifications before and after `initialize`'s answer, and the session's
/// mode updates before `session/new`'s.
fn devin_harness() -> Harness {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.route = AcpRoute::devin();
    h.control.config.executable_version = AcpRoute::devin().parse_version(VERSION).unwrap();
    let mut frames = h.injected.lock().unwrap();
    frames.push_back(serde_json::from_str(INITIALIZE).unwrap());
    frames.push_back(notification("_cognition.ai/mcp/serversChanged", json!({})));
    frames.push_back(notification(
        "_cognition.ai/output",
        json!({"channel":"<channel>","message":"<message>","level":"info","sessionId":""}),
    ));
    frames.push_back(notification(
        "session/update",
        json!({"sessionId":"<private-session>","update":{
            "sessionUpdate":"current_mode_update","currentModeId":"accept-edits"}}),
    ));
    frames.push_back(serde_json::from_str(NEW_SESSION).unwrap());
    frames.push_back(notification("_cognition.ai/mcp/serversChanged", json!({})));
    drop(frames);
    h
}

#[test]
fn a_chat_session_starts_without_authenticate_and_reads_modes_and_models() {
    let mut h = devin_harness();
    let caps = start(&mut h.control);
    assert_eq!(caps.provider.as_str(), "devin");
    assert_eq!(caps.executable_version.as_str(), "3000.10.48");
    assert!(caps.capabilities.supports(capability::PROVIDER_MODELS));
    // Devin has subagents and Koru has no switch for them: never
    // delegatable, which a chat does not need.
    assert_eq!(
        caps.delegation.child_sessions,
        ChildSessionPolicy::Uncontrolled
    );
    let models: Vec<_> = h
        .control
        .model_options()
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(models, ["model-a"]);
    let modes = h.control.modes().expect("Devin advertises its modes");
    assert_eq!(
        modes
            .modes()
            .iter()
            .map(|mode| mode.id.as_str())
            .collect::<Vec<_>>(),
        ["accept-edits", "smart", "ask", "plan", "bypass"]
    );
    assert_eq!(
        modes.current().map(|mode| mode.as_str()),
        Some("accept-edits")
    );
    let state = h.state.lock().unwrap();
    // `devin-browser` opens a browser; Devin's own sign-in applies.
    assert!(
        !state
            .sent()
            .iter()
            .any(|frame| frame["method"] == "authenticate")
    );
    assert!(!format!("{caps:?}").contains("private-session"));
}

/// **`agentInfo` is not Devin's version**: 3000.10.48 calls itself
/// `affogato 0.0.0-dev`, so the handshake does not compare it with the
/// printed version as Antigravity's does.
#[test]
fn agent_info_is_not_checked_against_the_printed_version() {
    let initialize: Value = serde_json::from_str(INITIALIZE).unwrap();
    assert_eq!(initialize["result"]["agentInfo"]["version"], "0.0.0-dev");
    let mut h = devin_harness();
    start(&mut h.control);
}

/// Only Devin picks among installed copies by version; every other route
/// runs what `PATH` resolves.
#[test]
fn only_devin_chooses_a_copy_by_version() {
    for (route, chooses) in [
        (AcpRoute::devin(), true),
        (AcpRoute::claude_code(), false),
        (AcpRoute::opencode(), false),
        (AcpRoute::kimi(), false),
        (AcpRoute::grok(), false),
        (AcpRoute::antigravity(), false),
    ] {
        assert_eq!(route.dialect.chooses_by_version(), chooses);
    }
    let route = AcpRoute::devin();
    assert!(
        route.dialect.exits_on_eof(),
        "measured: exits 0.13 s after EOF"
    );
    assert!(!route.dialect.unpacks_into_temp());
    assert_eq!(route.dialect.request_timeout_floor(), Duration::ZERO);
    assert_eq!(route.arguments, [std::ffi::OsString::from("acp")]);
    assert!(route.cached_auth_method.is_none());
}
