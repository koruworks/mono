// SPDX-License-Identifier: MPL-2.0

//! **An agent's approval modes** (koru-ai D7): what the agent says it does
//! before it asks a person — Claude's *default*, *acceptEdits*, *plan*, …,
//! OpenCode's *build* and *plan*, and the three Koru gives Codex, which
//! advertises none of its own.
//!
//! The ids are the agent's own and opaque here, like a model's: a surface
//! words the ones it knows and shows the agent's own name for the rest. The
//! list rides beside the model catalog, so a picker fills before a session
//! exists, and `current` is the mode the agent reports it starts in.

use std::{fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ModelLabel, TokenParseError};

/// The most modes one agent keeps; the rest are dropped.
pub const MAX_AGENT_MODES: usize = 16;
/// The longest mode id kept. Every id seen is a short word (`acceptEdits`).
pub const MAX_AGENT_MODE_ID_BYTES: usize = 64;

/// One agent's own id for one of its modes: printable ASCII with no space,
/// at most [`MAX_AGENT_MODE_ID_BYTES`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct AgentModeId(String);

impl AgentModeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AgentModeId {
    type Err = TokenParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        (!value.is_empty()
            && value.len() <= MAX_AGENT_MODE_ID_BYTES
            && value.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| Self(value.to_owned()))
        .ok_or(TokenParseError("invalid agent mode id"))
    }
}

impl TryFrom<String> for AgentModeId {
    type Error = TokenParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AgentModeId> for String {
    fn from(value: AgentModeId) -> Self {
        value.0
    }
}

/// A mode id is a word the agent publishes, never a person's content.
impl fmt::Debug for AgentModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AgentModeId({})", self.0)
    }
}

impl fmt::Display for AgentModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One mode, with what the agent itself says about it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentMode {
    pub id: AgentModeId,
    /// The agent's own name for it: ACP's mode `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<ModelLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<ModelLabel>,
}

impl AgentMode {
    pub fn new(id: AgentModeId) -> Self {
        Self {
            id,
            name: None,
            description: None,
        }
    }
}

impl fmt::Debug for AgentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentMode").field("id", &self.id).finish()
    }
}

/// **The modes an agent offers**, in the agent's order, and the one it
/// reports it is in. Never empty: an agent that offers none has no
/// `AgentModes` at all, and a surface shows no mode control for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "UncheckedModes", into = "UncheckedModes")]
pub struct AgentModes {
    modes: Vec<AgentMode>,
    current: Option<AgentModeId>,
}

impl AgentModes {
    /// The modes as listed, the first [`MAX_AGENT_MODES`] of them and none
    /// repeated, with `current` kept only when it is one of them. `None`
    /// when no mode is left.
    pub fn new(modes: Vec<AgentMode>, current: Option<AgentModeId>) -> Option<Self> {
        let mut kept: Vec<AgentMode> = Vec::new();
        for mode in modes {
            if kept.len() == MAX_AGENT_MODES {
                break;
            }
            if !kept.iter().any(|seen| seen.id == mode.id) {
                kept.push(mode);
            }
        }
        if kept.is_empty() {
            return None;
        }
        let current = current.filter(|current| kept.iter().any(|mode| mode.id == *current));
        Some(Self {
            modes: kept,
            current,
        })
    }

    pub fn modes(&self) -> &[AgentMode] {
        &self.modes
    }

    /// The mode the agent reports it is in, when it is one it listed.
    pub fn current(&self) -> Option<&AgentModeId> {
        self.current.as_ref()
    }

    pub fn contains(&self, id: &AgentModeId) -> bool {
        self.modes.iter().any(|mode| mode.id == *id)
    }

    /// The same list in another mode, which must be one of them.
    pub fn with_current(&self, current: &AgentModeId) -> Option<Self> {
        self.contains(current).then(|| Self {
            modes: self.modes.clone(),
            current: Some(current.clone()),
        })
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "AgentModes")]
struct UncheckedModes {
    modes: Vec<AgentMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<AgentModeId>,
}

impl TryFrom<UncheckedModes> for AgentModes {
    type Error = TokenParseError;

    /// Strict on the wire: a list a peer sends must already be what
    /// [`AgentModes::new`] would make of it.
    fn try_from(value: UncheckedModes) -> Result<Self, Self::Error> {
        let count = value.modes.len();
        let current = value.current.clone();
        match AgentModes::new(value.modes, value.current) {
            Some(modes) if modes.modes.len() == count && modes.current == current => Ok(modes),
            _ => Err(TokenParseError("invalid agent modes")),
        }
    }
}

impl From<AgentModes> for UncheckedModes {
    fn from(value: AgentModes) -> Self {
        Self {
            modes: value.modes,
            current: value.current,
        }
    }
}

#[cfg(test)]
mod tests;
