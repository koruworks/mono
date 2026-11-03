// SPDX-License-Identifier: MPL-2.0

//! Devin CLI as `devin acp`, replayed from 3000.10.48's sanitized fixtures
//! with the notifications it sends around them. The floor is 3000.11.1,
//! whose handshake is not recorded yet, so the frames run under the floor's
//! version and 3000.10.48's own print is refused.
use super::*;

const VERSION: &str = include_str!("fixtures/devin-3000.10.48/version.txt");
const INITIALIZE: &str = include_str!("fixtures/devin-3000.10.48/initialize.json");
const NEW_SESSION: &str = include_str!("fixtures/devin-3000.10.48/new-session.json");

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}

/// The signed-in `session/new` answer, or the signed-out one observed live
/// on 3000.10.48: the same session with a `model` select that lists
/// nothing (no fixture of it was kept; it differs only there, and in
/// `smart` arriving later).
fn new_session(signed_in: bool) -> Value {
    let mut answer: Value = serde_json::from_str(NEW_SESSION).unwrap();
    if !signed_in {
        let model = answer["result"]["configOptions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|option| option["id"] == "model")
            .unwrap();
        model["options"] = json!([]);
        model["currentValue"] = json!("");
    }
    answer
}

/// The frames in the order the session sent them: its own MCP
/// notifications before and after `initialize`'s answer, and the session's
/// mode updates before `session/new`'s.
fn devin_harness() -> Harness {
    session_harness(AcpRoute::devin(), true)
}

fn session_harness(route: AcpRoute, signed_in: bool) -> Harness {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.executable_version = route.supported_versions[0].clone();
    h.control.config.route = route;
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
    frames.push_back(new_session(signed_in));
    frames.push_back(notification("_cognition.ai/mcp/serversChanged", json!({})));
    drop(frames);
    h
}

#[test]
fn a_chat_session_starts_without_authenticate_and_reads_modes_and_models() {
    let mut h = devin_harness();
    let caps = start(&mut h.control);
    assert_eq!(caps.provider.as_str(), "devin");
    assert_eq!(caps.executable_version.as_str(), "3000.11.1");
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

/// **The floor is 3000.11.1**, newer admitted: the recorded 3000.10.48 is
/// refused at the version stage, which readiness reads as *Needs a
/// supported version*, and a session under it is refused before anything
/// is sent.
#[test]
fn the_floor_is_3000_11_1_and_the_recorded_build_is_below_it() {
    let route = AcpRoute::devin();
    assert_eq!(route.supported_versions[0].as_str(), "3000.11.1");
    let error = route.parse_version(VERSION).unwrap_err();
    assert_eq!(error.code, ErrorCode::ProviderIncompatible);
    assert_eq!(error.stage, ProviderStage::Version);
    assert_eq!(error.diagnostic, ProviderDiagnostic::VersionRejected);
    for (version, admitted) in [
        ("3000.10.21", false),
        ("3000.10.48", false),
        ("3000.11.0", false),
        ("3000.11.1", true),
        ("3000.11.234", true),
        ("3001.0.0", true),
    ] {
        assert_eq!(
            route.supports(&ProviderVersion::from_str(version).unwrap()),
            admitted,
            "{version}"
        );
    }
    let mut h = devin_harness();
    h.control.config.executable_version = ProviderVersion::from_str("3000.10.48").unwrap();
    let error = h
        .control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap_err();
    assert_eq!(error.diagnostic, ProviderDiagnostic::VersionRejected);
    assert!(h.state.lock().unwrap().sent().is_empty());
}

/// **A Devin session with no models is signed out.** Signed out, Devin
/// starts a session and lists no model; the readiness probe reads that as
/// `AuthenticationRequired`, which discovery classifies as *Signed out*.
#[test]
fn a_devin_session_with_no_models_reads_as_signed_out() {
    let mut h = session_harness(AcpRoute::devin(), false);
    start(&mut h.control);
    assert!(h.control.model_options().is_empty());
    let error = h
        .control
        .config
        .route
        .signed_in_by_models(h.control.model_options())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ProviderUnavailable);
    assert_eq!(error.diagnostic, ProviderDiagnostic::AuthenticationRequired);
    assert!(matches!(
        crate::providers::discovery::classify_failure(error),
        crate::providers::ManagedAvailability::SignedOut { .. }
    ));
    // Signed in, the same start lists its model and passes.
    let mut h = devin_harness();
    start(&mut h.control);
    assert!(
        h.control
            .config
            .route
            .signed_in_by_models(h.control.model_options())
            .is_ok()
    );
}

/// The same answer from any other agent is just an empty list: only
/// Devin's signed-out answer takes this shape.
#[test]
fn no_models_from_another_agent_is_not_signed_out() {
    let mut h = session_harness(AcpRoute::kimi(), false);
    start(&mut h.control);
    assert!(h.control.model_options().is_empty());
    assert!(
        h.control
            .config
            .route
            .signed_in_by_models(h.control.model_options())
            .is_ok()
    );
    for route in [
        AcpRoute::claude_code(),
        AcpRoute::opencode(),
        AcpRoute::kimi(),
        AcpRoute::grok(),
        AcpRoute::antigravity(),
    ] {
        assert!(!route.dialect.no_models_means_signed_out());
        assert!(route.signed_in_by_models(&[]).is_ok());
    }
    assert!(AcpRoute::devin().dialect.no_models_means_signed_out());
}
