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

/// An ACP peer that answers a start with a given `session/new` result.
struct ModePeer {
    new_session: Value,
    incoming: std::collections::VecDeque<Value>,
}

impl super::super::super::process::JsonRpcTransport for ModePeer {
    fn send(&mut self, message: &Value) -> Result<(), super::super::ProviderFailure> {
        let result = match message["method"].as_str().unwrap_or_default() {
            "initialize" => json!({"protocolVersion":1,"agentCapabilities":{},"authMethods":[]}),
            "session/new" => self.new_session.clone(),
            other => panic!("unexpected {other}"),
        };
        self.incoming
            .push_back(json!({"jsonrpc":"2.0","id":message["id"],"result":result}));
        Ok(())
    }
    fn receive(
        &mut self,
        _: std::time::Duration,
    ) -> Result<super::super::super::process::TransportReceive, super::super::ProviderFailure> {
        use super::super::super::process::TransportReceive;
        Ok(self
            .incoming
            .pop_front()
            .map_or(TransportReceive::Timeout, TransportReceive::Frame))
    }
    fn shutdown(&mut self, _: std::time::Duration) -> Result<(), super::super::ProviderFailure> {
        Ok(())
    }
}

fn started(new_session: Value) -> super::super::AcpControl {
    use super::super::{AcpConfig, AcpControl, AcpRoute};
    use crate::providers::{AgentControl, AgentStart};
    let config = AcpConfig::new(
        AcpRoute::opencode(),
        "fixture-opencode",
        std::path::PathBuf::from(if cfg!(windows) {
            "C:/project"
        } else {
            "/project"
        }),
        "1.18.27".parse().unwrap(),
        std::sync::Arc::new(crate::NoopAttachmentPathResolver),
    )
    .unwrap();
    let mut control = AcpControl::with_transport(
        config,
        Some(Box::new(ModePeer {
            new_session,
            incoming: Default::default(),
        })),
    );
    control
        .start(AgentStart::fresh(
            koru_session_protocol::SessionId::mint(),
            None,
        ))
        .unwrap();
    control
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
