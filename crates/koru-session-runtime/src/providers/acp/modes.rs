// SPDX-License-Identifier: MPL-2.0

//! **An ACP agent's approval modes** (koru-ai D7), as its session start or a
//! later answer advertises them.
//!
//! ACP says it two ways, and an agent may send either or both: the session's
//! `modes` — `{currentModeId, availableModes: [{id, name, description}]}` —
//! and a `select` config option whose `category` is `mode`. Claude's wrapper
//! (0.76.0) sends both, with the same ids (`default`, `acceptEdits`, `plan`,
//! `auto`, and `bypassPermissions` where it may); OpenCode sends its agent
//! personas (`build`, `plan`) as modes. The config option is preferred: it
//! is what `session/set_config_option` sets and what its answer reports.

use koru_session_protocol::{AgentMode, AgentModeId, AgentModes, ModelLabel};
use serde_json::Value;

/// What one answer says about the session's modes.
pub(super) struct SessionModes {
    pub modes: AgentModes,
    /// The `mode` config option's id when the agent advertised one: the
    /// option `session/set_config_option` names. `None` leaves
    /// `session/set_mode` as the way to change it.
    pub config_id: Option<String>,
}

/// The modes a `session/new` or `session/set_config_option` answer carries,
/// or `None` when it carries none — or more than one `mode` option, which
/// is no single answer.
pub(super) fn read(response: &Value) -> Option<SessionModes> {
    if let Some(option) = mode_option(response) {
        let mut modes = Vec::new();
        for entry in option["options"].as_array().into_iter().flatten() {
            match entry["options"].as_array() {
                Some(members) => {
                    modes.extend(members.iter().filter_map(|member| listed(member, "value")))
                }
                None => modes.extend(listed(entry, "value")),
            }
        }
        let current = option["currentValue"]
            .as_str()
            .and_then(|id| id.parse().ok());
        let config_id = option["id"].as_str().filter(|id| !id.is_empty())?;
        return AgentModes::new(modes, current).map(|modes| SessionModes {
            modes,
            config_id: Some(config_id.to_owned()),
        });
    }
    let state = response.get("modes")?;
    let modes = state["availableModes"]
        .as_array()?
        .iter()
        .filter_map(|mode| listed(mode, "id"))
        .collect();
    let current = state["currentModeId"]
        .as_str()
        .and_then(|id| id.parse().ok());
    AgentModes::new(modes, current).map(|modes| SessionModes {
        modes,
        config_id: None,
    })
}

/// The mode an answer or a `config_option_update` reports the session in:
/// the `mode` option's `currentValue`, else `modes.currentModeId`.
pub(super) fn current(response: &Value) -> Option<AgentModeId> {
    match mode_option(response) {
        Some(option) => option["currentValue"].as_str()?.parse().ok(),
        None => response
            .pointer("/modes/currentModeId")?
            .as_str()?
            .parse()
            .ok(),
    }
}

/// The one `select` option whose category is `mode`.
fn mode_option(response: &Value) -> Option<&Value> {
    let options = response.get("configOptions")?.as_array()?;
    let mut matching = options
        .iter()
        .filter(|option| option["category"] == "mode" && option["type"] == "select");
    let option = matching.next()?;
    matching.next().is_none().then_some(option)
}

/// One listed mode: its id under `key`, and the agent's own name and
/// description within `ModelLabel`'s bounds. An id that is not a mode id
/// is left out.
fn listed(entry: &Value, key: &str) -> Option<AgentMode> {
    let id = entry[key].as_str()?.parse().ok()?;
    let mut mode = AgentMode::new(id);
    mode.name = entry["name"].as_str().and_then(ModelLabel::name);
    mode.description = entry["description"]
        .as_str()
        .and_then(ModelLabel::description);
    Some(mode)
}

#[cfg(test)]
mod tests;
