// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde_json::json;

fn id(value: &str) -> AgentModeId {
    value.parse().unwrap()
}

#[test]
fn a_mode_id_is_a_short_printable_word() {
    assert!("acceptEdits".parse::<AgentModeId>().is_ok());
    assert!("full-access".parse::<AgentModeId>().is_ok());
    assert!("".parse::<AgentModeId>().is_err());
    assert!("two words".parse::<AgentModeId>().is_err());
    assert!("plan\n".parse::<AgentModeId>().is_err());
    assert!(
        "x".repeat(MAX_AGENT_MODE_ID_BYTES + 1)
            .parse::<AgentModeId>()
            .is_err()
    );
}

#[test]
fn modes_keep_order_drop_repeats_and_keep_current_only_when_listed() {
    let modes = AgentModes::new(
        vec![
            AgentMode::new(id("default")),
            AgentMode::new(id("plan")),
            AgentMode::new(id("default")),
        ],
        Some(id("plan")),
    )
    .unwrap();
    let ids: Vec<_> = modes.modes().iter().map(|mode| mode.id.as_str()).collect();
    assert_eq!(ids, ["default", "plan"]);
    assert_eq!(modes.current(), Some(&id("plan")));

    let unknown = AgentModes::new(vec![AgentMode::new(id("default"))], Some(id("auto"))).unwrap();
    assert_eq!(unknown.current(), None);
    assert!(AgentModes::new(Vec::new(), None).is_none());
    assert!(modes.with_current(&id("auto")).is_none());
    assert_eq!(
        modes.with_current(&id("default")).unwrap().current(),
        Some(&id("default"))
    );
}

#[test]
fn modes_round_trip_and_the_wire_refuses_what_new_would_change() {
    let mut plan = AgentMode::new(id("plan"));
    plan.name = ModelLabel::name("Plan");
    plan.description = ModelLabel::description("Reads and plans");
    let modes = AgentModes::new(
        vec![AgentMode::new(id("default")), plan],
        Some(id("default")),
    )
    .unwrap();
    let value = serde_json::to_value(&modes).unwrap();
    assert_eq!(
        value,
        json!({"modes":[{"id":"default"},{"id":"plan","name":"Plan","description":"Reads and plans"}],
            "current":"default"})
    );
    assert_eq!(serde_json::from_value::<AgentModes>(value).unwrap(), modes);

    for refused in [
        json!({"modes":[]}),
        json!({"modes":[{"id":"plan"},{"id":"plan"}]}),
        json!({"modes":[{"id":"plan"}],"current":"auto"}),
        json!({"modes":[{"id":"bad id"}]}),
    ] {
        assert!(serde_json::from_value::<AgentModes>(refused).is_err());
    }
}
