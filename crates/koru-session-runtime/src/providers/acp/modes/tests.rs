// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde_json::json;

const CLAUDE: &str = include_str!("../tests/fixtures/claude-0.76.0/new-session.sanitized.json");
const OPENCODE: &str = include_str!("../tests/fixtures/opencode-1.19.5/new-session.json");
const ANTIGRAVITY: &str = include_str!("../tests/fixtures/antigravity-1.2.1/new-session.json");

fn result(fixture: &str) -> Value {
    serde_json::from_str::<Value>(fixture).unwrap()["result"].clone()
}

fn ids(modes: &AgentModes) -> Vec<&str> {
    modes.modes().iter().map(|mode| mode.id.as_str()).collect()
}

/// Claude's wrapper 0.76.0 sends both `modes` and the `mode` option; the
/// option is read, with the wrapper's own names, and its id kept for the set.
#[test]
fn claude_advertises_its_five_modes_through_the_mode_option() {
    let read = read(&result(CLAUDE)).unwrap();
    assert_eq!(
        ids(&read.modes),
        [
            "default",
            "acceptEdits",
            "plan",
            "auto",
            "bypassPermissions"
        ]
    );
    assert_eq!(
        read.modes.current().map(AgentModeId::as_str),
        Some("default")
    );
    assert_eq!(read.config_id.as_deref(), Some("mode"));
    let accept = &read.modes.modes()[1];
    assert_eq!(
        accept.name.as_ref().map(ModelLabel::as_str),
        Some("Accept edits")
    );
    assert_eq!(
        accept.description.as_ref().map(ModelLabel::as_str),
        Some("Automatically accept all file edits")
    );
    assert_eq!(
        current(&result(CLAUDE)).as_ref().map(AgentModeId::as_str),
        Some("default")
    );
}

/// OpenCode 1.19.5 sends its personas as the `mode` option and no `modes`.
#[test]
fn opencode_advertises_build_and_plan() {
    let read = read(&result(OPENCODE)).unwrap();
    assert_eq!(ids(&read.modes), ["build", "plan"]);
    assert_eq!(read.modes.current().map(AgentModeId::as_str), Some("build"));
}

/// An agent that sends only ACP's `modes` object is read from it, and has
/// no option to set: `session/set_mode` is its way.
#[test]
fn the_modes_object_alone_is_read_and_has_no_config_id() {
    let session = json!({"sessionId":"s","modes":{"currentModeId":"ask","availableModes":[
        {"id":"ask","name":"Ask"},{"id":"code","name":"Code","description":"Writes code"}]}});
    let read = read(&session).unwrap();
    assert_eq!(ids(&read.modes), ["ask", "code"]);
    assert_eq!(read.config_id, None);
    assert_eq!(
        current(&session).as_ref().map(AgentModeId::as_str),
        Some("ask")
    );
    // Antigravity 1.2.1's sanitized answer has both, with the same three.
    assert_eq!(
        read_ids(ANTIGRAVITY).unwrap(),
        ["<mode-1>", "<mode-2>", "<mode-3>"]
    );
}

fn read_ids(fixture: &str) -> Option<Vec<String>> {
    read(&result(fixture)).map(|read| {
        read.modes
            .modes()
            .iter()
            .map(|mode| mode.id.as_str().to_owned())
            .collect()
    })
}

#[test]
fn an_answer_without_modes_or_with_two_mode_options_offers_none() {
    assert!(read(&json!({"sessionId":"s"})).is_none());
    assert!(read(&json!({"modes":{"availableModes":[]}})).is_none());
    let option = json!({"id":"mode","category":"mode","type":"select","currentValue":"a",
        "options":[{"value":"a"}]});
    assert!(read(&json!({"configOptions":[option.clone(), option]})).is_none());
}

use crate::providers::process::{JsonRpcTransport, TransportReceive};
use crate::providers::{AgentControl, AgentInput, AgentStart, ProviderFailure};
use koru_session_protocol::{AgentEvent, AgentTurnId, LaunchSelection};
use std::sync::{Arc, Mutex};

/// What a [`ModePeer`] was sent, and what it is told to do.
#[derive(Default)]
struct Script {
    sent: Vec<Value>,
    /// The mode as the last set left it.
    current: Option<String>,
    /// `(asked, landed)`: a set of `asked` lands on `landed`, as Claude's
    /// `auto` does on a model without it.
    lands: Option<(String, String)>,
    refuse: bool,
    /// A Fast mode set is answered with the option left as it was.
    refuse_fast: bool,
    /// Notifications sent ahead of the prompt's answer.
    during_turn: Vec<Value>,
}

/// An ACP peer that answers a start with a given `session/new` result, sets
/// the mode option or `session/set_mode` as told, and ends each prompt.
struct ModePeer {
    new_session: Value,
    script: Arc<Mutex<Script>>,
    incoming: std::collections::VecDeque<Value>,
}

fn update(kind: &str, body: Value) -> Value {
    let mut update = body;
    update["sessionUpdate"] = json!(kind);
    json!({"jsonrpc":"2.0","method":"session/update",
        "params":{"sessionId":"s","update":update}})
}

impl JsonRpcTransport for ModePeer {
    fn send(&mut self, message: &Value) -> Result<(), ProviderFailure> {
        let mut script = self.script.lock().unwrap();
        script.sent.push(message.clone());
        let id = message["id"].clone();
        let params = &message["params"];
        let result = match message["method"].as_str().unwrap_or_default() {
            "initialize" => json!({"protocolVersion":1,"agentCapabilities":{},"authMethods":[]}),
            "session/new" => self.new_session.clone(),
            "session/set_config_option" | "session/set_mode" if script.refuse => {
                self.incoming.push_back(json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32602,"message":"Invalid Mode"}}));
                return Ok(());
            }
            "session/set_config_option" if params["configId"] == "fast" => {
                let mut options = self.new_session["configOptions"].clone();
                for option in options.as_array_mut().unwrap() {
                    if option["id"] == "fast" && !script.refuse_fast {
                        option["currentValue"] = params["value"].clone();
                    }
                }
                self.new_session["configOptions"] = options.clone();
                json!({"configOptions": options})
            }
            "session/set_config_option" => {
                let asked = params["value"].as_str().unwrap().to_owned();
                let landed = match &script.lands {
                    Some((from, to)) if *from == asked => to.clone(),
                    _ => asked,
                };
                script.current = Some(landed.clone());
                // The wrapper says where it landed before it answers.
                let mut moved = update("current_mode_update", json!({"currentModeId": landed}));
                moved["params"]["sessionId"] = self.new_session["sessionId"].clone();
                self.incoming.push_back(moved);
                let mut options = self.new_session["configOptions"].clone();
                for option in options.as_array_mut().unwrap() {
                    if option["category"] == "mode" {
                        option["currentValue"] = json!(landed);
                    }
                }
                json!({"configOptions": options})
            }
            "session/set_mode" => {
                script.current = params["modeId"].as_str().map(str::to_owned);
                json!({})
            }
            "session/prompt" => {
                for mut notification in std::mem::take(&mut script.during_turn) {
                    notification["params"]["sessionId"] = self.new_session["sessionId"].clone();
                    self.incoming.push_back(notification);
                }
                json!({"stopReason":"end_turn"})
            }
            other => panic!("unexpected {other}"),
        };
        self.incoming
            .push_back(json!({"jsonrpc":"2.0","id":id,"result":result}));
        Ok(())
    }
    fn receive(&mut self, _: std::time::Duration) -> Result<TransportReceive, ProviderFailure> {
        Ok(self
            .incoming
            .pop_front()
            .map_or(TransportReceive::Timeout, TransportReceive::Frame))
    }
    fn shutdown(&mut self, _: std::time::Duration) -> Result<(), ProviderFailure> {
        Ok(())
    }
}

fn control(
    new_session: Value,
    mode: Option<&str>,
) -> (super::super::AcpControl, Arc<Mutex<Script>>) {
    use super::super::{AcpConfig, AcpControl, AcpRoute};
    let script = Arc::new(Mutex::new(Script::default()));
    let config = AcpConfig::new(
        AcpRoute::opencode(),
        "fixture-opencode",
        std::path::PathBuf::from(if cfg!(windows) {
            "C:/project"
        } else {
            "/project"
        }),
        "1.18.27".parse().unwrap(),
        Arc::new(crate::NoopAttachmentPathResolver),
    )
    .unwrap()
    .with_selection(mode.map(|mode| LaunchSelection {
        mode: Some(mode.parse().unwrap()),
        fast: None,
        ..Default::default()
    }));
    let control = AcpControl::with_transport(
        config,
        Some(Box::new(ModePeer {
            new_session,
            script: Arc::clone(&script),
            incoming: Default::default(),
        })),
    );
    (control, script)
}

fn start(control: &mut super::super::AcpControl) -> Result<(), ProviderFailure> {
    control
        .start(AgentStart::fresh(
            koru_session_protocol::SessionId::mint(),
            None,
        ))
        .map(|_| ())
}

fn started(new_session: Value) -> super::super::AcpControl {
    let (mut control, _) = control(new_session, None);
    start(&mut control).unwrap();
    control
}

fn turn(mode: Option<&str>) -> AgentInput {
    AgentInput {
        turn_id: AgentTurnId::mint(),
        text: "<prompt>".into(),
        attachments: Vec::new(),
        mode: mode.map(|mode| mode.parse().unwrap()),
        fast: None,
    }
}

/// Every event the adapter has for the running turn, to its end.
fn events(control: &mut super::super::AcpControl) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = control.next_event(std::time::Duration::ZERO).unwrap() {
        let done = matches!(event, AgentEvent::TurnCompleted { .. });
        events.push(event);
        if done {
            break;
        }
    }
    events
}

fn modes_changed(events: &[AgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ModeChanged { mode, .. } => Some(mode.as_str()),
            _ => None,
        })
        .collect()
}

fn methods(script: &Arc<Mutex<Script>>) -> Vec<String> {
    script
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter_map(|frame| frame["method"].as_str().map(str::to_owned))
        .collect()
}

/// A session's start keeps what it advertised, for the probe to hand the
/// catalog; one that advertised none has none.
#[test]
fn a_started_session_keeps_the_modes_it_advertised() {
    let control = started(result(CLAUDE));
    let modes = control.modes().unwrap();
    assert_eq!(modes.modes().len(), 5);
    assert_eq!(modes.current().map(AgentModeId::as_str), Some("default"));
    assert!(started(json!({"sessionId":"s"})).modes().is_none());
}

/// **A chosen mode is set at start through the mode option, and verified**
/// from its answer; the session is in it before any prompt.
#[test]
fn a_chosen_mode_is_set_at_start_and_verified() {
    let (mut control, script) = control(result(CLAUDE), Some("plan"));
    start(&mut control).unwrap();
    assert_eq!(
        control.modes().unwrap().current().map(AgentModeId::as_str),
        Some("plan")
    );
    let sent = script.lock().unwrap().sent.clone();
    let set = sent
        .iter()
        .find(|frame| frame["method"] == "session/set_config_option")
        .unwrap();
    assert_eq!(set["params"]["configId"], "mode");
    assert_eq!(set["params"]["value"], "plan");
    // The set's own update is its answer, not news for the next turn.
    control.submit(turn(None)).unwrap();
    assert!(modes_changed(&events(&mut control)).is_empty());
}

/// The mode the session is already in costs no call; one it does not offer,
/// or a refused set, fails the start before any prompt.
#[test]
fn a_mode_not_offered_or_refused_fails_the_start() {
    let (mut started, script) = control(result(CLAUDE), Some("default"));
    start(&mut started).unwrap();
    assert!(!methods(&script).contains(&"session/set_config_option".to_owned()));

    let (mut unknown, _) = control(result(OPENCODE), Some("acceptEdits"));
    let error = start(&mut unknown).unwrap_err();
    assert_eq!(error.code, koru_session_protocol::ErrorCode::InvalidRequest);

    let (mut refused, script) = control(result(CLAUDE), Some("plan"));
    script.lock().unwrap().refuse = true;
    assert!(start(&mut refused).is_err());
    assert!(!methods(&script).contains(&"session/prompt".to_owned()));
}

/// **A turn's mode is set before its prompt**, and a turn the agent cannot
/// move to is refused without a prompt.
#[test]
fn a_turns_mode_is_set_before_its_prompt() {
    let (mut control, script) = control(result(CLAUDE), None);
    start(&mut control).unwrap();
    control.submit(turn(Some("acceptEdits"))).unwrap();
    events(&mut control);
    let sent = methods(&script);
    let set = sent
        .iter()
        .position(|method| method == "session/set_config_option")
        .unwrap();
    let prompt = sent
        .iter()
        .position(|method| method == "session/prompt")
        .unwrap();
    assert!(set < prompt);

    script.lock().unwrap().refuse = true;
    let prompts = || {
        methods(&script)
            .iter()
            .filter(|method| *method == "session/prompt")
            .count()
    };
    let before = prompts();
    assert!(control.submit(turn(Some("plan"))).is_err());
    assert_eq!(prompts(), before);
}

/// An agent with only ACP's `modes` object is moved with `session/set_mode`.
#[test]
fn an_agent_without_the_option_is_moved_with_set_mode() {
    let session = json!({"sessionId":"s","modes":{"currentModeId":"ask","availableModes":[
        {"id":"ask"},{"id":"code"}]}});
    let (mut control, script) = control(session, Some("code"));
    start(&mut control).unwrap();
    assert!(methods(&script).contains(&"session/set_mode".to_owned()));
    assert_eq!(script.lock().unwrap().current.as_deref(), Some("code"));
    assert_eq!(
        control.modes().unwrap().current().map(AgentModeId::as_str),
        Some("code")
    );
}

/// **The chip follows the agent**: a mode the agent lands on instead of the
/// one asked is said with the next turn, and one it moves to during a turn
/// — Claude leaving plan mode — is said on that turn.
#[test]
fn a_mode_the_agent_moves_to_itself_is_said_on_a_turn() {
    let (mut landed, script) = control(result(CLAUDE), Some("auto"));
    script.lock().unwrap().lands = Some(("auto".into(), "acceptEdits".into()));
    start(&mut landed).unwrap();
    landed.submit(turn(None)).unwrap();
    assert_eq!(modes_changed(&events(&mut landed)), ["acceptEdits"]);

    let (mut planning, script) = control(result(CLAUDE), Some("plan"));
    start(&mut planning).unwrap();
    script.lock().unwrap().during_turn = vec![update(
        "config_option_update",
        json!({"configOptions":[{"id":"mode","category":"mode","type":"select",
            "currentValue":"default","options":[{"value":"default"},{"value":"plan"}]}]}),
    )];
    planning.submit(turn(None)).unwrap();
    assert_eq!(modes_changed(&events(&mut planning)), ["default"]);
    assert_eq!(
        planning.modes().unwrap().current().map(AgentModeId::as_str),
        Some("default")
    );
}

fn fast_control(fast: Option<bool>) -> (super::super::AcpControl, Arc<Mutex<Script>>) {
    let (mut control, script) = control(result(CLAUDE), None);
    if let Some(on) = fast {
        control.config.selection = Some(LaunchSelection {
            fast: Some(on),
            ..Default::default()
        });
    }
    (control, script)
}

fn fast_sets(script: &Arc<Mutex<Script>>) -> Vec<Value> {
    script
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|frame| {
            frame["method"] == "session/set_config_option" && frame["params"]["configId"] == "fast"
        })
        .map(|frame| frame["params"]["value"].clone())
        .collect()
}

/// **Fast mode at start and per turn** (koru-ai D11): Claude's `fast`
/// select is set to `on` at start and verified, set back to `off` before a
/// turn's prompt, and a turn that leaves it alone sends nothing.
#[test]
fn fast_mode_is_set_at_start_and_per_turn_through_its_option() {
    let (mut control, script) = fast_control(Some(true));
    start(&mut control).unwrap();
    assert_eq!(fast_sets(&script), [json!("on")]);
    let mut off = turn(None);
    off.fast = Some(false);
    control.submit(off).unwrap();
    events(&mut control);
    assert_eq!(fast_sets(&script), [json!("on"), json!("off")]);
    control.submit(turn(None)).unwrap();
    events(&mut control);
    assert_eq!(fast_sets(&script).len(), 2, "no choice, no call");
}

/// A model with no Fast mode ignores the choice; a refused set refuses the
/// turn before its prompt.
#[test]
fn fast_mode_is_ignored_without_the_option_and_a_refusal_refuses_the_turn() {
    let (mut control, script) = control(result(OPENCODE), None);
    control.config.selection = Some(LaunchSelection {
        fast: Some(true),
        ..Default::default()
    });
    start(&mut control).unwrap();
    assert!(fast_sets(&script).is_empty());

    let (mut control, script) = fast_control(None);
    start(&mut control).unwrap();
    script.lock().unwrap().refuse_fast = true;
    let mut on = turn(None);
    on.fast = Some(true);
    assert!(control.submit(on).is_err());
    assert!(!methods(&script).contains(&"session/prompt".to_owned()));
}
